//! Properties of the section layer.
//!
//! Two invariants carry the whole PSI stack, and both are round trips that no
//! finite set of named cases can cover: a section survives *any* split across
//! transport packets, and a CRC appended to a body makes the residue zero for
//! *any* body.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use proptest::prelude::*;
use vaco_format_mpegts_tables::crc::{RESIDUE, crc32, section_crc_ok};
use vaco_format_mpegts_tables::section::{
    MAX_SECTION_LEN, Section, SectionAssembler, SectionHeader,
};

/// Build a long-form section with a valid CRC around `body`.
fn section(table_id: u8, ext: u16, version: u8, body: &[u8]) -> Vec<u8> {
    let section_length = 5 + body.len() + 4;
    let mut s = vec![
        table_id,
        0xB0 | ((section_length >> 8) as u8 & 0x0F),
        (section_length & 0xFF) as u8,
        (ext >> 8) as u8,
        (ext & 0xFF) as u8,
        0xC1 | ((version & 0x1F) << 1),
        0,
        0,
    ];
    s.extend_from_slice(body);
    s.extend_from_slice(&crc32(&s).to_be_bytes());
    s
}

/// Split a run of sections into transport payloads exactly as a muxer does:
/// a `payload_unit_start_indicator` with a `pointer_field` wherever a section
/// begins, and no indicator on a continuation.
fn deliver(sections: &[Vec<u8>], payload_len: usize) -> Vec<(bool, Vec<u8>)> {
    let mut data = Vec::new();
    let mut starts = Vec::new();
    for s in sections {
        starts.push(data.len());
        data.extend_from_slice(s);
    }
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < data.len() {
        let next_start = starts.iter().copied().find(|&s| s >= at);
        let pusi = next_start.is_some_and(|s| s < at + payload_len.saturating_sub(1));
        // A continuation packet must stop short of the next section start,
        // or that section begins inside a packet with no `pointer_field` and
        // no reader could ever find it. A real muxer has the same constraint.
        let cap = if pusi {
            payload_len.saturating_sub(1)
        } else {
            next_start.map_or(payload_len, |s| (s - at).min(payload_len))
        };
        if cap == 0 {
            break;
        }
        let n = cap.min(data.len() - at);
        let mut payload = Vec::new();
        if pusi {
            payload.push((next_start.unwrap() - at) as u8);
        }
        payload.extend_from_slice(&data[at..at + n]);
        out.push((pusi, payload));
        at += n;
    }
    out
}

proptest! {
    /// A section arrives intact however the transport chopped it up.
    ///
    /// This is the assembler's entire contract, and it is where every
    /// off-by-one in `pointer_field` handling shows up.
    #[test]
    fn a_section_survives_any_split(
        body in proptest::collection::vec(any::<u8>(), 0..900),
        payload_len in 8usize..=184,
        table_id in any::<u8>(),
        version in 0u8..32,
    ) {
        // 0xFF is the stuffing byte, so a section can never start with it.
        let table_id = if table_id == 0xFF { 0x00 } else { table_id };
        let want = section(table_id, 7, version, &body);
        let mut asm = SectionAssembler::new();
        let mut got: Vec<Vec<u8>> = Vec::new();
        for (pusi, payload) in deliver(std::slice::from_ref(&want), payload_len) {
            asm.push(pusi, &payload, |s| got.push(s.to_vec()));
        }
        prop_assert_eq!(got.len(), 1);
        prop_assert_eq!(&got[0], &want);
        let parsed = Section::new(&got[0]).unwrap();
        prop_assert!(parsed.crc_ok());
        prop_assert_eq!(parsed.header.version, version);
        prop_assert_eq!(parsed.body().unwrap(), &body[..]);
    }

    /// Several sections in a row arrive in order and none is lost, whatever
    /// the packet size — including the case where one packet holds the tail of
    /// one section and the head of the next.
    #[test]
    fn a_run_of_sections_arrives_in_order(
        bodies in proptest::collection::vec(
            proptest::collection::vec(any::<u8>(), 0..120),
            1..8,
        ),
        payload_len in 8usize..=184,
    ) {
        let want: Vec<Vec<u8>> = bodies
            .iter()
            .enumerate()
            .map(|(i, b)| section(0x02, i as u16, (i % 32) as u8, b))
            .collect();
        let mut asm = SectionAssembler::new();
        let mut got: Vec<Vec<u8>> = Vec::new();
        for (pusi, payload) in deliver(&want, payload_len) {
            asm.push(pusi, &payload, |s| got.push(s.to_vec()));
        }
        prop_assert_eq!(&got, &want);
    }

    /// Abandoning mid-section loses exactly the section in progress and
    /// nothing after it, which is what a continuity gap must cost.
    #[test]
    fn abandoning_loses_one_section_and_recovers(
        bodies in proptest::collection::vec(
            proptest::collection::vec(any::<u8>(), 20..80),
            2..6,
        ),
    ) {
        let want: Vec<Vec<u8>> = bodies
            .iter()
            .map(|b| section(0x02, 1, 0, b))
            .collect();
        let payloads = deliver(&want, 40);
        let mut asm = SectionAssembler::new();
        let mut got: Vec<Vec<u8>> = Vec::new();
        for (i, (pusi, payload)) in payloads.iter().enumerate() {
            if i == 1 {
                asm.abandon();
            }
            asm.push(*pusi, payload, |s| got.push(s.to_vec()));
        }
        // Everything from the second section on must still arrive.
        prop_assert!(got.len() >= want.len() - 1, "recovery lost more than one section");
        for s in &got {
            prop_assert!(want.contains(s), "emitted a section nobody sent");
        }
    }

    /// The assembler never emits anything the length field cannot describe,
    /// and never emits a section whose own header disagrees with its length.
    #[test]
    fn arbitrary_payloads_never_break_the_framing_invariant(
        pushes in proptest::collection::vec(
            (any::<bool>(), proptest::collection::vec(any::<u8>(), 0..190)),
            1..40,
        ),
    ) {
        let mut asm = SectionAssembler::new();
        let mut total = 0usize;
        for (pusi, payload) in &pushes {
            total += payload.len();
            asm.push(*pusi, payload, |raw| {
                assert!(raw.len() <= MAX_SECTION_LEN);
                if let Some(h) = SectionHeader::parse(raw) {
                    assert_eq!(h.total_len(), raw.len());
                } else {
                    assert!(raw.len() < 8);
                }
            });
        }
        let stats = asm.stats();
        prop_assert!(stats.emitted <= total as u64);
    }

    /// Appending the CRC makes the residue zero, and flipping any single bit
    /// breaks it. The second half is the property that matters: a checksum
    /// that accepts everything is worse than none.
    #[test]
    fn crc_detects_every_single_bit_error(
        body in proptest::collection::vec(any::<u8>(), 1..200),
        bit in 0usize..8,
        pos in 0usize..200,
    ) {
        let mut s = body.clone();
        s.extend_from_slice(&crc32(&body).to_be_bytes());
        prop_assert!(section_crc_ok(&s));
        prop_assert_eq!(crc32(&s), RESIDUE);
        let at = pos % s.len();
        s[at] ^= 1 << bit;
        prop_assert!(!section_crc_ok(&s), "bit {} of byte {} went undetected", bit, at);
    }
}
