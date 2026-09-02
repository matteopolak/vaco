//! Fixture verification harness.
//!
//! Every fixture below is one of two kinds, and each says which:
//!
//! - **Hand-built**: bytes assembled directly from this crate's own reading
//!   of the CEA-608/CEA-708 code tables (see `src/cea608/tables.rs` and
//!   `src/cea708/tables.rs`, which cite where those tables came from). This
//!   is self-verification against the spec tables the decoder itself uses,
//!   not a comparison against an independent reference decoder.
//! - **Real-world**: raw `cc_data` bytes extracted with `PyAV`
//!   (`frame.side_data`, `Type.A53_CC`) from a genuine broadcast capture
//!   (`transformers_EIA608_H264.ts`, an EIA-608 conformance sample
//!   published at `samples.ffmpeg.org/ffmpeg-bugs/trac/ticket2885/`, used
//!   here only as an unmodified byte source, not as code). This is the
//!   stronger oracle: the expected text is what a human reading the
//!   broadcast's captions would see, and it was cross-checked by manually
//!   walking the same bytes through the CEA-608 tables by hand before this
//!   fixture was written, so the decoder and the expectation were derived
//!   independently.
//!
//! `cargo test -p vaco-codec-subtitle-cc --test fixtures -- --nocapture`
//! prints the comparison table.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code"
)]

use vaco_codec_subtitle_cc::{CcDecoder, Event};

struct Fixture {
    name: &'static str,
    cc_data: Vec<u8>,
    expected: &'static str,
}

/// A `cc_data` triplet marker byte: marker bits `11111`, `cc_valid` set, and
/// the given `cc_type`.
const fn marker(cc_type: u8) -> u8 {
    0xF8 | 0x04 | (cc_type & 0b11)
}

const FIELD1: u8 = 0; // cc_type 0b00
const START: u8 = 3; // cc_type 0b11
const CONT: u8 = 2; // cc_type 0b10

fn odd_parity(byte: u8) -> u8 {
    if byte.count_ones() % 2 == 1 {
        byte
    } else {
        byte | 0x80
    }
}

/// One field-1 CEA-608 triplet from a raw (unparitized) byte pair.
fn cc608(cc_data: &mut Vec<u8>, b0: u8, b1: u8) {
    cc_data.push(marker(FIELD1));
    cc_data.push(odd_parity(b0));
    cc_data.push(odd_parity(b1));
}

fn hand_built_pop_on() -> Fixture {
    let mut cc_data = Vec::new();
    cc608(&mut cc_data, 0x14, 0x20); // RCL
    cc608(&mut cc_data, 0x11, 0x40); // PAC row 1, white
    cc608(&mut cc_data, b'H', b'E');
    cc608(&mut cc_data, b'L', b'L');
    cc608(&mut cc_data, b'O', 0x00);
    cc608(&mut cc_data, 0x14, 0x2F); // EOC
    Fixture {
        name: "hand-built: pop-on HELLO",
        cc_data,
        expected: "HELLO",
    }
}

fn hand_built_roll_up() -> Fixture {
    let mut cc_data = Vec::new();
    cc608(&mut cc_data, 0x14, 0x25); // RU2
    cc608(&mut cc_data, 0x14, 0x60); // PAC row 15, white (base row for roll-up)
    cc608(&mut cc_data, b'R', b'O');
    cc608(&mut cc_data, b'L', b'L');
    cc608(&mut cc_data, 0x14, 0x2D); // CR: scroll, "ROLL" moves to row 14
    cc608(&mut cc_data, b'U', b'P');
    Fixture {
        name: "hand-built: roll-up ROLL / UP",
        cc_data,
        expected: "ROLL\nUP",
    }
}

fn hand_built_styling_and_special_char() -> Fixture {
    let mut cc_data = Vec::new();
    cc608(&mut cc_data, 0x14, 0x20); // RCL
    cc608(&mut cc_data, 0x11, 0x40); // PAC row 1, white
    cc608(&mut cc_data, 0x11, 0x28); // mid-row: red, no underline (writes a space)
    cc608(&mut cc_data, b'H', b'I');
    cc608(&mut cc_data, 0x11, 0x30); // special char: registered trademark sign
    cc608(&mut cc_data, 0x14, 0x2F); // EOC
    Fixture {
        name: "hand-built: mid-row style + special char",
        cc_data,
        expected: " HI\u{00AE}",
    }
}

/// DTVCC packet, service 1: `DefineWindow0` (visible), `SetCurrentWindow0`,
/// then `G0` text "OK".
fn hand_built_dtvcc_window() -> Fixture {
    let define_args = [0x20u8, 0x00, 0x00, 0x00, 0x10, 0x00];
    let mut service_block = vec![0x98u8];
    service_block.extend_from_slice(&define_args);
    service_block.push(0x80); // SetCurrentWindow0
    service_block.push(b'O');
    service_block.push(b'K');

    let block_size = u8::try_from(service_block.len()).unwrap_or(31);
    let mut payload = vec![(1u8 << 5) | block_size];
    payload.extend_from_slice(&service_block);
    if payload.len() % 2 == 0 {
        payload.push(0x00);
    }
    let size_code = u8::try_from(payload.len().div_ceil(2)).unwrap_or(0);

    let mut cc_data = Vec::new();
    cc_data.push(marker(START));
    cc_data.push(size_code);
    cc_data.push(payload[0]);
    let mut i = 1;
    while i < payload.len() {
        cc_data.push(marker(CONT));
        cc_data.push(payload[i]);
        cc_data.push(*payload.get(i + 1).unwrap_or(&0));
        i += 2;
    }

    Fixture {
        name: "hand-built: DTVCC DefineWindow + text OK",
        cc_data,
        expected: "OK",
    }
}

/// Raw `cc_data` from `transformers_EIA608_H264.ts` frames 8-24 (0-indexed),
/// exactly as `PyAV` reported them via `frame.side_data` — see the module doc.
fn real_world_transformers() -> Fixture {
    let hex = "fc9420fd8080fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fc9420fd8080fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fc94f2fd8080ff472bfe9c18fe4a00fe001ffe2192fe0004fe0300fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fc94f2fd8080fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fc91aefd8080fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fc91aefd8080fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fce9f4fd8080ff8323fe6974fe0300fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fc7320fd8080ffc323fe7320fe0300fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fce3e9fd8080ff0323fe6369fe0300fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fcf4e9fd8080ff4323fe7469fe0300fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fce573fd8080ff8323fe6573fe0300fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fc206efd8080ffc323fe206efe0300fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fceff7fd8080ff0323fe6f77fe0300fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fcae80fd8080ff4222fe2e03fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fc942cfd8080fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fc942cfd8080fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000\
fc942ffd8080ff8425fe8a0ffe89f0fe0300fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000fa0000";
    let cc_data = decode_hex(hex);
    Fixture {
        name: "real-world: transformers_EIA608_H264.ts, \"its cities now.\"",
        cc_data,
        expected: " its cities now.",
    }
}

fn decode_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| hex.get(i..i + 2))
        .filter_map(|byte| u8::from_str_radix(byte, 16).ok())
        .collect()
}

/// Feed one fixture's `cc_data` through a fresh decoder, one frame at a
/// time (this fixture's bytes already come pre-split at NTSC frame
/// boundaries by the marker triplets they contain, so one call is enough
/// for the hand-built cases; the real-world fixture concatenates several
/// frames' `cc_data`, which is exactly what a real per-frame `feed` loop
/// would also produce one call at a time — feeding it in one shot here is
/// equivalent because this crate's triplet loop has no cross-call state
/// beyond what `CcDecoder` already carries).
fn last_text(fixture: &Fixture) -> String {
    let mut dec = CcDecoder::default();
    let mut last = String::new();
    for event in dec.feed(&fixture.cc_data) {
        if let Event::Cea608 { screen, .. }
        | Event::Cea708 {
            screen: Some(screen),
            ..
        } = event
            && !screen.is_empty()
        {
            last = screen.text();
        }
    }
    last
}

#[test]
fn fixture_table() {
    let fixtures = [
        hand_built_pop_on(),
        hand_built_roll_up(),
        hand_built_styling_and_special_char(),
        hand_built_dtvcc_window(),
        real_world_transformers(),
    ];

    println!(
        "{:<55} {:<30} {:<30} match",
        "fixture", "expected", "actual"
    );
    let mut all_matched = true;
    for fixture in &fixtures {
        let actual = last_text(fixture);
        let matched = actual == fixture.expected;
        all_matched &= matched;
        println!(
            "{:<55} {:<30} {:<30} {}",
            fixture.name,
            format!("{:?}", fixture.expected),
            format!("{actual:?}"),
            if matched { "y" } else { "n" }
        );
    }
    assert!(
        all_matched,
        "one or more fixtures did not match; see table above"
    );
}
