//! Real, measured codec configuration-record bytes for container test
//! suites — one place to hold a convention several independent crates'
//! tests otherwise hand-copy.
//!
//! # What it is
//!
//! `vaco-mux-matroska`, `vaco-demux-ogg`, `vaco-mux-ogg` and `vaco-cli` each
//! hand-synthesize container fixtures for their own tests, and at least one
//! real convention — Opus's `OpusHead` — had drifted into five byte-for-byte
//! identical `const` copies across those crates. `4ec43cc` (2026-09-01)
//! correctly started requiring `A_OPUS` tracks to carry a real `OpusHead` in
//! Matroska; it updated `vaco-mux-matroska`'s own copy but had no way to
//! know `vaco-cli`'s separate fixture existed, and that one went nine and a
//! half hours before anyone noticed (`planning/E2E-GAPS.md` #35). This crate
//! is the fix for the *shape* of that bug, not just the one instance.
//!
//! # How it works
//!
//! Plain `pub const` byte slices, one module per codec, each documented with
//! exactly how it was obtained. No parsing, no builder API — a container
//! fixture only needs *a* valid instance of a config record, not a way to
//! construct arbitrary ones, and a real measured example is more honest than
//! a synthetic one a test author might get subtly wrong.
//!
//! # How to change it
//!
//! Never edit a constant's bytes without re-measuring against the real
//! encoder/reference this doc's provenance note names — these are exhibits,
//! not scratch data. Adding a new one: measure it (`ffmpeg`/the relevant
//! reference tool), document the exact command and interpretation the same
//! way [`opus::HEAD_MONO`] does, and land it in its own module. If MP4 ever
//! needs the same treatment, `vaco-format-isom` already plays this role for
//! real box-writing code — this crate is deliberately test-only and does not
//! duplicate that.
//!
//! # Configuration
//!
//! None — pure constant data.
//!
//! # Dependencies
//!
//! None, deliberately: every container test crate across every layer this
//! tree has (`format`, `app`) can take this as a dev-dependency without
//! creating a cycle.
#![forbid(unsafe_code)]

/// Opus (RFC 7845) fixtures.
pub mod opus {
    /// A minimal, real `OpusHead` identification header: version 1, 1
    /// channel (mono), `pre_skip` 312, input sample rate 48000 Hz, output
    /// gain 0, channel mapping family 0.
    ///
    /// Measured against `ffmpeg -c:a libopus` (the same 19 bytes
    /// `vaco-mux-ogg`'s own round-trip test originally measured this
    /// against, D19 — not re-measured per copy). Valid as `CodecPrivate` in
    /// Matroska/WebM, as an Ogg page's identification packet, or wherever
    /// else a container needs *a* real `OpusHead` and does not care about
    /// its specific channel count.
    ///
    /// A fixture that needs a specific channel count or mapping family
    /// (multistream, surround) is not a candidate for consolidation here —
    /// build it locally and say why, the way
    /// `vaco-parse-opus`'s own multistream test does.
    pub const HEAD_MONO: &[u8] = &[
        b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 0x01, 0x01, 0x38, 0x01, 0x80, 0xBB, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];

    #[cfg(test)]
    #[allow(
        clippy::indexing_slicing,
        reason = "test code: a panic is the assertion mechanism"
    )]
    mod tests {
        use super::HEAD_MONO;

        #[test]
        fn head_mono_is_the_expected_nineteen_bytes() {
            assert_eq!(HEAD_MONO.len(), 19);
            assert_eq!(&HEAD_MONO[..8], b"OpusHead");
            assert_eq!(HEAD_MONO[8], 1, "version");
            assert_eq!(HEAD_MONO[9], 1, "channel count (mono)");
            assert_eq!(
                u16::from_le_bytes([HEAD_MONO[10], HEAD_MONO[11]]),
                312,
                "pre_skip"
            );
            assert_eq!(
                u32::from_le_bytes([HEAD_MONO[12], HEAD_MONO[13], HEAD_MONO[14], HEAD_MONO[15]]),
                48_000,
                "input sample rate"
            );
            assert_eq!(HEAD_MONO[18], 0, "channel mapping family 0");
        }
    }
}
