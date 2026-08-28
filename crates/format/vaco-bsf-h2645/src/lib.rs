//! H.264/HEVC bitstream filters.
//!
//! # What this is
//!
//! `h264_mp4toannexb` and `hevc_mp4toannexb` — the two filters
//! `vaco-mux-avi` and `vaco-mux-mpegts` are waiting on (their own inline
//! length-prefix-to-Annex-B converters do the framing half but never splice
//! parameter sets back in front of a keyframe, which the reference does; see
//! each module's docs for the measurement). `h264_metadata`/`hevc_metadata`
//! (issue #353, B-05) are here too, as the measured identity transform —
//! see their own module docs for why. `h264_redundant_pps` and `dts2pts` are
//! not implemented — see their sections below for why.
//!
//! # The CBS write path — what exists, and what does not (issue #353)
//!
//! `vaco-codec-cbs` has the *shape* of a write path:
//! `CbsCodec::{read_unit, write_unit, assemble}` and `Cbs::{update_unit,
//! insert_unit}` are all real, general APIs. But the only `CbsCodec`
//! implementation for either codec in this tree, `vaco_parse_hevc::cbs::HevcCbs`,
//! can only `write_unit` a raw (undecoded) unit back out — every typed
//! variant (`Sps`, `Pps`, `Vps`, `Sei`) returns `Error::Unsupported`, by that
//! module's own design (see its docs: a non-bit-exact parameter-set writer
//! "silently corrupts a stream rather than failing"). `vaco-parse-h264` has
//! no `CbsCodec` implementation at all. So: **the write path is scaffolded,
//! not built** — nobody has written a bit-exact H.264 or HEVC SPS/PPS
//! serialiser yet, and that is the real, unstarted work B-05's title points
//! at.
//!
//! Whether `h264_metadata`/`hevc_metadata` need that work *today* is a
//! separate question, answered in their own module docs: measured directly,
//! every option either filter exposes defaults to "leave the bitstream
//! alone", and gap 12 below means no option can reach a filter instance
//! anyway — so the two filters registered here are the reference's own
//! bare-name behaviour, verified byte-identical, with no write path
//! involved. Building the SPS/PPS writer now would have no caller in this
//! workspace able to exercise it with anything but the default value, which
//! is exactly the "dead code no test can honestly cover" trap
//! `vaco-bsf-av1::metadata`'s docs already name. Left unbuilt, and the gap
//! recorded rather than worked around silently.
//!
//! # Gap 12 (`BsfProvider::open` has no option string) — not closed here
//!
//! `planning/INTERFACE-GAPS.md` gap 12 is the reason every option on
//! `h264_metadata`/`hevc_metadata` (and `dts2pts`, if it existed) is
//! unreachable. It is a trait method, not a bare fn pointer, so — mirroring
//! how gaps 4/5/6 were substituted the same day this crate was extended, by
//! adding a defaulted `Muxer::set_option` — it could plausibly be closed by a
//! defaulted `BitstreamFilter::set_option(&mut self, name: &str, value: &str)
//! -> Result<()>` (default: `Err(Error::Unsupported(..))`), called after
//! `open` and before the first packet. That trait lives in
//! `vaco-codec-core`, which is not a crate this issue's owner has standing to
//! edit (single-writer rule) — so the shape is recorded here, in
//! `planning/INTERFACE-GAPS.md`, and in the issue-closing report, rather than
//! applied silently. Until it lands (and a CLI-side `-bsf:v name=opts`
//! parser is wired to call it — a separate, larger piece of work gap 12's
//! own text already flags as out of scope), every `*_metadata` filter in
//! this tree is the bare-name behaviour and nothing else. That is still
//! worth registering exactly where the bare-name behaviour is *measured*
//! identity, which is the case for both filters below.
//!
//! # How it works
//!
//! Framing (length-prefixed ↔ Annex B) is
//! `vaco_format_nalu::convert::length_prefixed_to_annexb`, not reimplemented
//! here (that crate's own module docs name this crate as the place its
//! "everything else" — parameter-set splicing — belongs). Parameter sets come
//! from `vaco_parse_h264::AvcDecoderConfigurationRecord` /
//! `vaco_parse_hevc::HevcDecoderConfigurationRecord`, parsed once at
//! construction from `CodecParameters::extradata`. Splicing is a byte-level
//! NAL-unit insertion, using `vaco_format_nalu::units` to find the insertion
//! point rather than scanning start codes by hand.
//!
//! # `h264_redundant_pps` — measured, not implemented
//!
//! Measured against `ffmpeg 8.1` on an x264 stream with `repeat-headers=1`
//! (which emits two PPS occurrences per keyframe): the filter's effect is not
//! "delete the second PPS NAL unit" at the byte level. A `SequenceMatcher`
//! diff of the filtered and unfiltered elementary streams shows the edit
//! starts *inside* the surviving PPS's own RBSP (a handful of bits shorter),
//! and small, recurring, non-byte-aligned differences continue through the
//! following slice's CABAC-coded data before resetting at the next NAL
//! boundary — the signature of a bit width changing mid-stream (most likely
//! `pic_parameter_set_id`'s `ue(v)` encoding, if the surviving PPS's id
//! differs from the one a slice used to reference) rather than of a clean
//! unit deletion.
//!
//! Reproducing that needs a CABAC-safe, bit-precise PPS rewrite and slice
//! header renumbering — the same class of problem
//! `vaco_parse_hevc::cbs::HevcCbs`'s own docs call out as *not yet
//! supported*, for the identical reason: "writing an SPS means writing
//! ... bit-exactly, and a writer that is not bit-exact silently corrupts a
//! stream rather than failing." `vaco-parse-h264` has no bit-writer layer at
//! all (unlike HEVC's `cbs` module), so there is nowhere to build this
//! correctly today. Shipping a naive byte-level unit removal without the
//! renumbering would produce a stream real decoders reject or misdecode,
//! which is worse than not registering the filter — left out rather than
//! landed wrong.
//!
//! # `dts2pts` — measured, not implemented (issue #354)
//!
//! Its name suggests DTS audio; `ffmpeg -h bsf=dts2pts` reports `Supported
//! codecs: h264 hevc` instead — "dts" here is *decode timestamp*, not the
//! codec (`vaco-bsf-audio`'s docs already record this same correction). It
//! touches no bitstream bytes at all, only `Packet::pts`, so it does not need
//! the CBS write path above.
//!
//! What it needs instead, measured directly: fed a raw H.264 elementary
//! stream (every packet `pts:NOPTS`, monotonically increasing `dts` in decode
//! order, default `libx264` B-frame settings), `-bsf:v dts2pts` assigns a
//! distinct, non-trivial `pts` to every packet. It is **not** a fixed
//! reorder-delay shift: `pts[3] == dts[3]` (no delay) while `pts[0] ==
//! dts[2]`, `pts[2] == dts[4]` (delay 2) and `pts[1]` matches a `dts` value
//! four packets further out than that shift would predict. That pattern is
//! what a real picture-order-count computation over a hierarchical B-frame
//! structure produces (H.264 §8.2.1 has three separate POC types; HEVC §8.3.1
//! has its own), not a constant offset — ruling out the tempting
//! shortcut before it got shipped.
//!
//! Building this correctly means decoding slice-header POC fields per §8.2.1
//! (H.264) or §8.3.1 (HEVC), buffering a reorder window, and re-emitting
//! `dts` values in POC order as `pts` — a decoder-adjacent task on the order
//! of the CBS write path itself, not a "thin layer" on top of existing
//! parsing. `vaco-parse-h264`/`vaco-parse-hevc` parse slice headers already
//! but this crate does not yet drive that path into a reorder buffer, and a
//! reorder policy validated against only the one GOP structure above would
//! be exactly the "one matching sample is not a passing test" trap this
//! project's own findings warn about — a hierarchical-B pattern with
//! different reference distances would silently reassign the wrong picture's
//! timestamp. Left unimplemented rather than shipped on an unverified guess
//! at the general rule; membership (H.264/HEVC, not audio) is corrected here
//! even though the filter itself is not.
//!
//! # How to change it
//!
//! Add a module, implement [`vaco_bsf_core::PacketMap`], export a `DESC`, add
//! it to `filters()`, and register it with a `[[component]]` table in
//! `vaco-component.toml`.
//!
//! # Configuration
//!
//! None — see `vaco-bsf-generic`'s crate docs for why (`BsfProvider::open`
//! carries no option string).
//!
//! # Dependencies
//!
//! `vaco-bsf-core` for the driver; `vaco-format-nalu` for framing and NAL
//! headers; `vaco-parse-h264`/`vaco-parse-hevc` for decoder configuration
//! record parsing.

#![forbid(unsafe_code)]

pub mod h264_metadata;
pub mod h264_mp4toannexb;
pub mod hevc_metadata;
pub mod hevc_mp4toannexb;

/// Every filter this crate registers.
#[must_use]
pub fn filters() -> &'static [vaco_bsf_core::BsfDesc] {
    &[
        h264_metadata::DESC,
        h264_mp4toannexb::DESC,
        hevc_metadata::DESC,
        hevc_mp4toannexb::DESC,
    ]
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
