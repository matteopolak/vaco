//! `VorbisComment` and `Picture` over arbitrary bytes.
//!
//! Both are declared-length-prefixed formats read from container metadata a
//! file's author controls — exactly plan 13 §2.2.2's "declared length"
//! amplification shape `vaco-parse-opus`'s comment-header fuzz target
//! exists for, and this crate's comment reader is the same wire shape.
//!
//! fuzz-crate: vaco-format-vorbiscomment
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_vorbiscomment::{Picture, VorbisComment};

fuzz_target!(|data: &[u8]| {
    if let Ok(comment) = VorbisComment::parse_raw(data) {
        let yielded = comment.iter().count();
        assert_eq!(yielded as u64, u64::from(comment.len()));
        let bytes: usize = comment.iter().map(str::len).sum();
        assert!(bytes <= data.len());
        for (k, _) in comment.pairs() {
            assert!(!k.contains('='));
        }
    }
    let _ = VorbisComment::parse_native(data);
    if let Ok(picture) = Picture::parse(data) {
        assert!(picture.data.len() <= data.len());
        assert!(picture.mime_type.len() <= data.len());
        assert!(picture.description.len() <= data.len());
    }
});
