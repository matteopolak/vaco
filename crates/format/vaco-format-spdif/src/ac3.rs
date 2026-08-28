//! The minimal fixed-position slice of an AC-3 sync frame header this crate
//! needs to declare a stream — not a general AC-3 parser (there is no
//! `vaco-parse-ac3` in this workspace to reuse or duplicate).
//!
//! # What was measured
//!
//! Against the raw bytes of `ffmpeg -f lavfi -i sine ... -c:a ac3 out.ac3`
//! (bitrate 192 kb/s, 48 kHz, stereo): `0B 77 9C B1 14 40 43 E1 ...`.
//! Manually decoding per the publicly documented ATSC A/52 sync-frame
//! layout and cross-checking against the known encode parameters:
//!
//! * bytes 0-1: sync word `0x0B77` — checked here too, not just upstream by
//!   the burst's own data-type field, since [`parse`] is also what
//!   `SpdifMuxer::write_packet` uses to validate a caller-supplied frame.
//! * bytes 2-3: `crc1`, not read here.
//! * byte 4, top 2 bits (`fscod`): `0b00` = 48 kHz — matches the 48 kHz
//!   source. The remaining 6 bits (`frmsizecod`) are not read; this crate
//!   gets the frame length from the burst's own `Pd` field instead (see
//!   `iec61937.rs`), not by recomputing it from a size table.
//! * byte 6, top 3 bits (`acmod`): `0b010` = 2 — matches the `-ac 2` source.
//!
//! # What is deliberately not read
//!
//! `lfeon` (whether a low-frequency-effects channel is present) sits after
//! `acmod` at a bit offset that depends on `acmod`'s own value
//! (`cmixlev`/`surmixlev`/`dsurmod` are present or absent depending on which
//! matrix mode `acmod` selects), so reading it correctly needs a real bit
//! cursor, not a fixed byte offset. This module reports the `acmod` channel
//! count table only — a 5.1 stream reports 5 channels, not 6. Documented,
//! not guessed at: see `docs/format/vaco-format-spdif.md`.

/// `fscod` -> sample rate, straight off the ATSC A/52 sync-frame table.
const SAMPLE_RATE_BY_FSCOD: [Option<u32>; 4] = [Some(48_000), Some(44_100), Some(32_000), None];

/// `acmod` -> channel count, **not including LFE** (see module docs).
const CHANNELS_BY_ACMOD: [u16; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

/// The subset of an AC-3 sync frame's header this crate can state with
/// confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ac3Header {
    pub sample_rate: u32,
    /// Does not add the LFE channel — see module docs.
    pub channels: u16,
}

/// Read [`Ac3Header`] from the start of an AC-3 elementary-stream frame.
///
/// `None` if `payload` is too short or `fscod` is the reserved value `3`.
#[must_use]
pub(crate) fn parse(payload: &[u8]) -> Option<Ac3Header> {
    if payload.get(0..2)? != [0x0B, 0x77] {
        return None;
    }
    let byte4 = *payload.get(4)?;
    let byte6 = *payload.get(6)?;
    let fscod = usize::from((byte4 >> 6) & 0b11);
    let acmod = usize::from((byte6 >> 5) & 0b111);
    let sample_rate = (*SAMPLE_RATE_BY_FSCOD.get(fscod)?)?;
    let channels = *CHANNELS_BY_ACMOD.get(acmod)?;
    Some(Ac3Header {
        sample_rate,
        channels,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;

    /// The exact bytes measured in the module docs.
    #[test]
    fn the_measured_192kbps_48khz_stereo_header_decodes_correctly() {
        let bytes = [0x0B, 0x77, 0x9C, 0xB1, 0x14, 0x40, 0x43, 0xE1];
        let h = parse(&bytes).expect("a valid header");
        assert_eq!(h.sample_rate, 48_000);
        assert_eq!(h.channels, 2);
    }

    #[test]
    fn a_reserved_fscod_is_rejected() {
        let mut bytes = [0x0B, 0x77, 0x9C, 0xB1, 0x14, 0x40, 0x43, 0xE1];
        bytes[4] |= 0b1100_0000; // fscod = 3, reserved
        assert!(parse(&bytes).is_none());
    }

    #[test]
    fn a_short_buffer_is_rejected_not_panicked_on() {
        assert!(parse(&[0x0B, 0x77]).is_none());
    }

    #[test]
    fn a_missing_sync_word_is_rejected() {
        assert!(parse(&[0u8; 16]).is_none());
    }
}
