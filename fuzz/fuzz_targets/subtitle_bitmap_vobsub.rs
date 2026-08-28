//! Arbitrary bytes as one DVD subpicture (SPU) unit, through
//! `vobsub::decode_spu` -- the SPU header, `SP_DCSQT` control-sequence walk
//! (including the self-referencing-chain termination and `SET_DAREA`/
//! `SET_DSPXA` offsets) and the 4/8/12/16-bit nibble-escalation run-length
//! grammar, all of which read attacker-controlled offsets and run lengths.
//! The palette is fixed: in the real pipeline it arrives out-of-band (a
//! `.idx` file or Matroska `CodecPrivate`, not the SPU bytes themselves --
//! see `vobsub.rs`'s module docs), so it is not part of what this target
//! fuzzes.
//!
//! fuzz-crate: vaco-codec-subtitle-bitmap

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_subtitle_bitmap::vobsub;
use vaco_format_subtitle_bitmap::{Palette, Rgba};
use vaco_limits::Limits;

fuzz_target!(|data: &[u8]| {
    let palette = Palette::new(vec![Rgba::new(0, 0, 0, 255); 16]).unwrap_or_default();
    let _ = vobsub::decode_spu(data, &palette, &Limits::permissive());
});
