//! `MpegAudioHeader`, `XingHeader`, `LameTag` and `VbriHeader` over arbitrary
//! bytes: none of these take a budget (they only read fixed-size fields out
//! of a caller-supplied slice, never allocate), so panic-freedom on any
//! length including zero is most of what there is to check.
//!
//! `MpegAudioHeader` is the exception: its own doc comment states an exact
//! invariant its `to_word`/`to_bytes` inverse has never been checked
//! against here -- "the inverse of `parse`: `parse(h.to_word())`
//! round-trips for every `h` `parse` itself could have produced." Every
//! field maps to its own non-overlapping bit range (no reserved/padding
//! bits shared between fields the way a packed pixel format can have), so
//! unlike this session's `vaco-scale`/`vaco-pixfmt` work this round trip
//! needs no representation carve-outs: a re-encoded header must decode back
//! to the identical struct.
//!
//! fuzz-crate: vaco-format-mpegaudio

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_mpegaudio::{LameTag, MpegAudioHeader, VbriHeader, XingHeader};

fuzz_target!(|data: &[u8]| {
    if let Some(chunk) = data.first_chunk::<4>() {
        let word = u32::from_be_bytes(*chunk);
        if let Some(header) = MpegAudioHeader::parse(word) {
            let _ = header.frame_len();
            let _ = header.side_info_len();
            let bytes = header.to_bytes();
            let re_word = u32::from_be_bytes(bytes);
            let redecoded = MpegAudioHeader::parse(re_word)
                .expect("a header's own re-encoding must itself parse as a header");
            assert_eq!(
                redecoded, header,
                "parse(h.to_word()) did not round-trip for {header:?}"
            );
        }
    }
    let _ = MpegAudioHeader::parse_bytes(data);
    let _ = XingHeader::parse(data);
    let _ = LameTag::parse(data);
    let _ = VbriHeader::parse(data);
});
