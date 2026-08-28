//! Legacy and professional-format bitstream filters.
//!
//! # What this is
//!
//! Issue #354 (B-06)'s "legacy" half, plus the two `*_metadata` filters left
//! over from issue #353 (B-05) once `h264_metadata`/`hevc_metadata` claimed
//! `vaco-bsf-h2645` and `av1_metadata`/`vp9_metadata`/`opus_metadata` were
//! already homed in their own per-codec crates: `mpeg2_metadata` and
//! `prores_metadata`. Both are the measured identity transform, for the
//! identical reason as their siblings — see each module's docs.
//!
//! # What was measured and left out
//!
//! `ffmpeg -bsfs` names several more filters this crate's "legacy" mandate
//! would plausibly cover. Each was checked against `ffmpeg 8.1` and left
//! unregistered, for a reason specific to it rather than a blanket "too old":
//!
//! * **`mjpeg2jpeg`** does more than the option table would suggest — it has
//!   no options at all, so there is no "default behaviour" to fall back on.
//!   Measured on a real `mjpeg` elementary stream: it inserts a `DHT` marker
//!   carrying the ITU-T T.81 Annex K.3 standard Huffman tables immediately
//!   after the leading `APP0`, **and** it rewrites that `APP0`'s JFIF version
//!   and density fields (`01.02`/units `2`/`1×1` density in the source became
//!   `01.01`/units `0`/`0×0` in the output). The DHT insertion is a clean,
//!   spec-derived, always-the-same-bytes operation; the JFIF rewrite is not
//!   obviously either "always overwrite with these exact constants" or
//!   "derive somehow from the input" from one sample, and this environment's
//!   `mjpeg` encoder has no option that varies JFIF version or density
//!   independently of everything else, so there is no second sample to tell
//!   the two hypotheses apart. Shipping the constant-overwrite guess on one
//!   data point is exactly the "one matching sample is not a passing test"
//!   trap this project's findings warn about twice over (a `sine` formula
//!   and a `compensationdelay` formula, both confirmed by a single case and
//!   wrong more generally). Left out.
//! * **`mjpegadump`** inserts a 40-byte `APP1` marker containing the ASCII
//!   tag `mjpg` and two repeated 4-byte fields, measured on the same stream —
//!   but again from exactly one sample, with no visible way in this
//!   environment to vary whatever those two fields encode (field dominance?
//!   a frame byte count? unclear from one data point) independently of the
//!   rest of the frame. Left out for the same single-sample reason.
//! * **`imxdump`** (`Supported codecs: mpeg2video`) is meant for Sony XDCAM
//!   IMX/D-10 streams specifically — a fixed-bitrate, intra-only, 4:2:2
//!   MPEG-2 profile this environment's native `mpeg2video` encoder has no
//!   option set for. Run on an ordinary `mpeg2video` stream it is **not**
//!   identity (it diverges from the first byte), which at minimum confirms
//!   the filter is not a no-op gated on stream shape the way the profile
//!   name might suggest — but a divergence on non-IMX input is not evidence
//!   of *correct* IMX-specific behaviour, and there is no real D-10 stream in
//!   this environment to measure against instead. Left out rather than
//!   generalised from a sample that is not even the intended input shape.
//! * **`dovi_rpu`** (`Supported codecs: hevc av1`) needs a real Dolby Vision
//!   RPU-bearing elementary stream, which requires proprietary metadata this
//!   environment's encoders cannot embed. No oracle input, so no
//!   measurement to build from — the same shape as `vaco-bsf-vpx`'s
//!   documented `vp9_raw_reorder` exclusion.
//! * **`dv_error_marker`** (`Supported codecs: dvvideo`) draws a solid-colour
//!   error-concealment block based on per-macroblock error-status bytes
//!   real capture hardware writes when a DV deck reports a dropout. There is
//!   no such damaged footage to synthesise here, and its eighteen-value
//!   `sta` flag set is exactly the kind of enumeration a single
//!   error-free synthetic clip cannot exercise even one branch of honestly.
//! * **`evc_frame_merge`** (`Supported codecs: evc`) has no oracle: this
//!   `ffmpeg` build has an EVC *decoder* but no EVC *encoder*
//!   (`ffmpeg -encoders` confirms it), so there is no way to produce a real
//!   EVC elementary stream to measure against.
//! * **`hapqa_extract`** (`Supported codecs: hap`) and **`media100_to_mjpegb`**
//!   (`Supported codecs: media100`) name codecs with no
//!   [`vaco_codec_core::CodecId`] variant in this workspace — unreachable,
//!   not merely unimplemented, the same call `vaco-bsf-audio` made for
//!   `ahx_to_mp2`.
//!
//! `apv_metadata`, `lcevc_metadata` and `vvc_metadata`/`vvc_mp4toannexb`
//! (from #353/#354's wider `*_metadata` and framing families) are the same
//! two shapes: APV and LCEVC have no `CodecId` in this workspace at all
//! (unreachable), and VVC has a `CodecId` but this `ffmpeg` build has a VVC
//! *decoder* only, no encoder, and no VVC sample was available to measure
//! against — so `vvc_metadata`'s single `aud`-only option table (which would
//! otherwise suggest an easy identity case, same as `h264_metadata`) was
//! never actually checked against a real VVC bitstream, and is left
//! unregistered rather than assumed from the option table alone.
//!
//! `h264_redundant_pps`'s exclusion is `vaco-bsf-h2645`'s call, not this
//! crate's — see that crate's docs.
//!
//! # How it works
//!
//! Same shape as every other `vaco-bsf-*` crate: one [`vaco_bsf_core::BsfDesc`]
//! per module, built on [`vaco_bsf_core::PacketMap`] wrapped in
//! [`vaco_bsf_core::MappedFilter`].
//!
//! # Configuration
//!
//! None reachable: [`vaco_format_core::mux::BsfProvider::open`] has no
//! per-instance option string (`planning/INTERFACE-GAPS.md` gap 12). See
//! `vaco-bsf-h2645`'s crate docs for the fuller account of why that gap is
//! recorded rather than closed by this issue's owner, and why the filters
//! here are worth registering anyway: every option either filter exposes is
//! measured to default to "leave the bitstream alone", so the bare-name
//! behaviour this interface limits us to is also the *correct*, verified
//! behaviour.
//!
//! # Dependencies
//!
//! `vaco-bsf-core` for the driver; `vaco-codec-core` for [`vaco_codec_core::CodecId`]
//! and [`vaco_codec_core::CodecParameters`]. No codec-specific parsing crate:
//! both filters here are pure identity, gated only on `codec_id`.

#![forbid(unsafe_code)]

pub mod mpeg2_metadata;
pub mod prores_metadata;

/// Every filter this crate registers.
#[must_use]
pub fn filters() -> &'static [vaco_bsf_core::BsfDesc] {
    &[mpeg2_metadata::DESC, prores_metadata::DESC]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_filter_has_a_unique_name() {
        let names: Vec<&str> = filters().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names.len(), sorted.len(), "{names:?}");
    }
}
