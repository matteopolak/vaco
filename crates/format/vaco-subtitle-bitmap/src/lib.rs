//! Bitmap subtitle demuxers and muxers: `dvbsub`, `dvbtxt`, `sup` (Blu-ray
//! PGS) and `vobsub` (DVD subpicture).
//!
//! # Scope (D17 — measured against `ffmpeg 8.1`, not the plan's count)
//!
//! `ffmpeg -demuxers | grep -iE 'sub|sup|vob|dvb|pgs'` names four demuxers in
//! this family beyond the text-subtitle ones `vaco-subtitle-text` already
//! covers: `dvbsub`, `dvbtxt`, `sup`, `vobsub` — matching FM-52's list
//! exactly (unlike several sibling briefs this wave, whose plan-stated counts
//! were off by one against the measured reference). `ffmpeg -muxers` over the
//! same filter names exactly **one** of the four with a muxer: `sup`. `dvd`
//! and `vob` also match that grep, but they are MPEG-PS container muxers (the
//! *transport* for `vobsub`'s codec, not a `vobsub`-format muxer), out of
//! this crate's scope, and `dvbsub`/`dvbtxt`/`vobsub` genuinely have none.
//!
//! | | Demux | Mux | `CodecId` |
//! |---|---|---|---|
//! | [`dvbsub`] | yes | no (reference has none) | `CodecId::DvbSubtitle` |
//! | [`dvbtxt`] | yes | no | `CodecId::DvbTeletext` |
//! | [`sup`] | yes | yes | `CodecId::HdmvPgsSubtitle` |
//! | [`vobsub`] | yes | no | `CodecId::DvdSubtitle` |
//!
//! All four `CodecId` variants already existed in `vaco-codec-core` before
//! this crate — unlike `vaco-subtitle-text`'s family, there was no gap to
//! report here.
//!
//! # The demuxer/decoder line — read the module you need, but the short
//! version is
//!
//! A bitmap subtitle stream carries compressed regions: run-length-encoded
//! pixels, palettes, display timing. This crate frames packets and recovers
//! their timing; it never runs a run-length decompressor. Where that line
//! falls is genuinely different for each of the four formats, and each
//! module's own docs explain the specific measurement or specification
//! reasoning behind its placement:
//!
//! * [`dvbsub`] and [`dvbtxt`] are, as standalone elementary streams,
//!   measurably just raw chunk readers in the reference (`ffmpeg -h
//!   demuxer=dvbsub`/`dvbtxt` both name the generic raw demuxer's own
//!   `-raw_packet_size` option) — real DVB subtitle/teletext delivery already
//!   has its framing done by MPEG-TS, which is a different crate's scope
//!   entirely. Both still get real ETSI-spec-based structural parsers
//!   ([`dvbsub::segments`], [`dvbtxt::teletext`]) for probing and for a future
//!   decoder to use, kept separate from packetisation on purpose.
//! * [`sup`] genuinely does have its own container-level segment framing (a
//!   `"PG"` magic, a PTS/DTS, a type, a size, repeated) with no standalone-ES
//!   fallback in the reference, so [`sup::DEMUXER`] frames on it directly.
//! * [`vobsub`] splits down the middle of its own two files: the `.idx` is
//!   plain text and fully this crate's job ([`vobsub::idx`]); the `.sub`
//!   payload is MPEG-PS `private_stream_1`, which `vaco-demux-mpegps` already
//!   demuxes, so [`vobsub::VobSubDemuxer::open_pair`] uses it rather than
//!   re-deriving PES framing. See [`vobsub`]'s module docs for the frozen
//!   `DemuxerDesc::open` seam this format runs into as a result.
//!
//! # The shared bitmap model
//!
//! `vaco-format-subtitle-bitmap` (a separate crate — see its own docs) holds
//! the one shape all four formats' eventual decoders converge on: a palette,
//! a rectangle, and indexed pixel data. This crate uses its [`Rect`] and
//! [`Palette`] wherever a container states either directly (a `.idx`
//! `size:`/`palette:` line, a DVB region/CLUT segment's fixed header fields)
//! — never for actual decompressed pixels, which stay a decoder's job.
//!
//! [`Rect`]: vaco_format_subtitle_bitmap::Rect
//! [`Palette`]: vaco_format_subtitle_bitmap::Palette
//!
//! # Dependencies
//!
//! `vaco-format-core`, `vaco-codec-core`, `vaco-io`, `vaco-limits`,
//! `vaco-packet`, `vaco-format-subtitle-bitmap` (the shared model), and
//! `vaco-demux-mpegps` (for `vobsub`'s `.sub` half — a same-layer format
//! crate dependency, not a layering violation: D14.1 forbids a format crate
//! depending on a *codec* crate, not on another format crate, and
//! `vaco-demux-avi` depending on `vaco-format-riff` is the existing precedent
//! for exactly this shape).

#![forbid(unsafe_code)]

pub mod bytes;
pub mod dvbsub;
pub mod dvbtxt;
pub mod sup;
pub mod vobsub;
