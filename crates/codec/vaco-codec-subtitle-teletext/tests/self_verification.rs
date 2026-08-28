//! Self-verification fixtures for `vaco-codec-subtitle-teletext`.
//!
//! There is no ordinary path to synthesise a real DVB teletext elementary
//! stream: `ffmpeg` has a teletext **decoder** but no teletext **encoder**,
//! and no small public-domain `.ts` sample containing a real DVB teletext
//! stream was reachable from this environment (checked: ffmpeg's own
//! FATE-suite sample tree, the `xavery/ttxinfo` and `orryverducci/
//! TtxFromTS` repositories' own test assets — neither bundles one; the
//! `CCExtractor` project's teletext recordings exist only as large,
//! interstitial-gated Google Drive folders). So every fixture below is
//! hand-built directly from EN 300 706's own encoding equations (Hamming
//! 8/4, Hamming 24/18, odd parity) — this is self-verification against the
//! specification's own tables, **not** a diff against a reference binary's
//! output. Say so plainly rather than implying otherwise.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use vaco_codec_subtitle_teletext::page::Glyph;
use vaco_codec_subtitle_teletext::{Color, TeletextDecoder};

fn hamming_byte(nibble: u8) -> u8 {
    let d1 = nibble & 1;
    let d2 = (nibble >> 1) & 1;
    let d3 = (nibble >> 2) & 1;
    let d4 = (nibble >> 3) & 1;
    let p1 = 1 ^ d1 ^ d3 ^ d4;
    let p2 = 1 ^ d1 ^ d2 ^ d4;
    let p3 = 1 ^ d1 ^ d2 ^ d3;
    let p4 = 1 ^ p1 ^ d1 ^ p2 ^ d2 ^ p3 ^ d3 ^ d4;
    (p1 & 1)
        | ((d1 & 1) << 1)
        | ((p2 & 1) << 2)
        | ((d2 & 1) << 3)
        | ((p3 & 1) << 4)
        | ((d3 & 1) << 5)
        | ((p4 & 1) << 6)
        | ((d4 & 1) << 7)
}

fn parity_byte(data: u8) -> u8 {
    let d = data & 0x7F;
    if d.count_ones() % 2 == 1 {
        d
    } else {
        d | 0x80
    }
}

/// One EN 300 472 46-byte data unit wrapping one EN 300 706 packet.
fn data_unit(magazine: u8, packet_no: u8, control_and_text: &[u8]) -> Vec<u8> {
    let address = u16::from(magazine & 0x7) | (u16::from(packet_no) << 3);
    let byte4 = hamming_byte((address & 0xF) as u8);
    let byte5 = hamming_byte(((address >> 4) & 0xF) as u8);
    let mut record = vec![0x02u8, 0x2C, 0xC0, 0xE4, byte4, byte5];
    record.extend_from_slice(control_and_text);
    while record.len() < 46 {
        record.push(parity_byte(b' '));
    }
    record.truncate(46);
    record
}

fn header_control_bytes(page_units: u8, page_tens: u8) -> [u8; 8] {
    [
        hamming_byte(page_units),
        hamming_byte(page_tens),
        hamming_byte(0), // S1
        hamming_byte(0), // S2 + C4
        hamming_byte(0), // S3
        hamming_byte(0), // S4 + C5,C6
        hamming_byte(0), // C7-C10
        hamming_byte(0), // C11-C14
    ]
}

fn page_text(rows: &[[vaco_codec_subtitle_teletext::Cell; 40]], row: usize, start: usize, len: usize) -> String {
    rows[row][start..start + len]
        .iter()
        .map(|c| match c.glyph {
            Glyph::Text(ch) => ch,
            Glyph::Mosaic { .. } => '#',
            Glyph::Corrupt => '\u{FFFD}',
        })
        .collect()
}

/// Fixture 1: page 100, header plus one plain body row.
#[test]
fn fixture_1_basic_level_1_page() {
    let mut decoder = TeletextDecoder::new();
    let mut header = header_control_bytes(0, 0).to_vec(); // page 100
    header.extend("100 SELF-TEST".bytes().map(parity_byte));
    decoder.push(&data_unit(1, 0, &header));

    let row1: Vec<u8> = "HELLO WORLD".bytes().map(parity_byte).collect();
    decoder.push(&data_unit(1, 1, &row1));

    // Close the page with a fresh header for the same magazine.
    let closer = header_control_bytes(0, 0);
    let events = decoder.push(&data_unit(1, 0, &closer));

    assert_eq!(events.len(), 1);
    let page = &events[0].page;
    assert_eq!(page.page_number, 0x00);
    assert_eq!(page_text(&page.rows, 0, 8, 13), "100 SELF-TEST");
    assert_eq!(page_text(&page.rows, 1, 0, 11), "HELLO WORLD");
}

/// Fixture 2: English national-option substitution (the pound sign at code
/// point `0x23`, per Table 36's English row) inside a coloured body row.
#[test]
fn fixture_2_english_national_option_and_colour() {
    let mut decoder = TeletextDecoder::new();
    decoder.push(&data_unit(2, 0, &header_control_bytes(0, 2))); // page 200

    let mut body = vec![0x01u8]; // Alpha Red
    body.extend("PRICE ".bytes());
    body.push(0x23); // -> '£' under the English national-option table
    body.extend("12.50".bytes());
    let body: Vec<u8> = body.into_iter().map(parity_byte).collect();
    decoder.push(&data_unit(2, 1, &body));

    let events = decoder.push(&data_unit(2, 0, &header_control_bytes(0, 2)));
    assert_eq!(events.len(), 1);
    let page = &events[0].page;
    assert_eq!(page.rows[1][1].fg, Color::Red); // "PRICE" is red (set-after col 0)
    assert_eq!(page_text(&page.rows, 1, 1, 5), "PRICE");
    assert_eq!(page.rows[1][7].glyph, Glyph::Text('\u{A3}')); // 0x23 -> '£'
    assert_eq!(page_text(&page.rows, 1, 8, 5), "12.50");
}

/// Fixture 3: a single-bit Hamming error injected into the header's
/// page-units byte is corrected rather than corrupting the page number —
/// self-verifying the forward error correction this crate's `hamming`
/// module implements, over a hand-built packet rather than a captured one.
#[test]
fn fixture_3_hamming_correction_survives_a_bit_flip() {
    let mut decoder = TeletextDecoder::new();
    let mut header = header_control_bytes(0x7, 0x3).to_vec(); // page 0x37
    if let Some(byte) = header.first_mut() {
        *byte ^= 0x04; // flip one bit of the Hamming-coded units byte
    }
    header.extend("370".bytes().map(parity_byte));
    decoder.push(&data_unit(3, 0, &header));

    let events = decoder.push(&data_unit(3, 0, &header_control_bytes(0, 0)));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].page.page_number, 0x37);
    assert_eq!(events[0].page.corrupt_hamming, 0);
}
