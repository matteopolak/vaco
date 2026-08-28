//! The RIFF chunk layer, and the two structures everything built on RIFF
//! needs: `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE` and `BITMAPINFOHEADER`.
//!
//! This is **not** a demuxer. It registers no component (`vaco-registry`
//! finds demuxers, not the chunk parser underneath them) — `vaco-demux-wav`
//! and `vaco-demux-avi` are the eventual demuxers, built on this the way
//! `vaco-demux-mp4` is built on `vaco-format-isom`. It is also, per
//! `planning/20-roadmap.md`'s framing of this whole crate family, the crate
//! that unblocks `vaco-demux-matroska`'s `V_MS/VFW/FOURCC` and `A_MS/ACM`
//! tracks: a Matroska `CodecPrivate` for either of those is exactly a
//! `BITMAPINFOHEADER` or a `WAVEFORMATEX`, stored verbatim, with no RIFF
//! chunk wrapper around it — which is why [`bitmapinfo::BitmapInfoHeader`]
//! and [`wave::WaveFormatEx`] parse from a plain byte slice rather than
//! requiring a `fmt ` chunk to have been read first.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`chunk`] | `RIFF`/`LIST` chunk headers, flat iteration, word-alignment padding |
//! | [`rf64`] | the `RF64`/`ds64` 64-bit-size extension |
//! | [`wave`] | `WAVEFORMATEX` / `WAVEFORMATEXTENSIBLE` |
//! | [`bitmapinfo`] | `BITMAPINFOHEADER` and `biCompression` |
//! | [`wave_tags`] | `wFormatTag` → codec name / [`vaco_codec_core::CodecId`] |
//! | [`video_tags`] | `biCompression` → codec name / [`vaco_codec_core::CodecId`] |
//! | [`info`] | `LIST`/`INFO` chunk tags (`ISFT` → `encoder`) |
//! | [`info`] | `LIST`/`INFO` chunk tags (`ISFT` → `encoder`) |
//!
//! # Example
//!
//! Walking a WAV file's top-level chunks and reading its format:
//!
//! ```
//! use vaco_format_riff::chunk::{RiffHeader, ids};
//! use vaco_format_riff::wave::WaveFormatEx;
//! use vaco_format_riff::wave_tags;
//! use vaco_limits::{Budget, Limits};
//!
//! # let file = {
//! #     let mut fmt_payload = vec![1, 0, 1, 0, 0x44, 0xac, 0, 0, 0x88, 0x58, 1, 0, 2, 0, 16, 0];
//! #     let mut body = Vec::new();
//! #     body.extend_from_slice(b"fmt ");
//! #     body.extend_from_slice(&(fmt_payload.len() as u32).to_le_bytes());
//! #     body.append(&mut fmt_payload);
//! #     body.extend_from_slice(b"data");
//! #     body.extend_from_slice(&4u32.to_le_bytes());
//! #     body.extend_from_slice(&[0; 4]);
//! #     let mut file = b"RIFF".to_vec();
//! #     file.extend_from_slice(&((4 + body.len()) as u32).to_le_bytes());
//! #     file.extend_from_slice(b"WAVE");
//! #     file.extend_from_slice(&body);
//! #     file
//! # };
//! let header = RiffHeader::parse(&file)?;
//! assert_eq!(header.form_type, ids::WAVE);
//!
//! let fmt_chunk = header.children(&file).find(ids::FMT).expect("a fmt chunk");
//! let mut budget = Budget::new(Limits::permissive());
//! let fmt = WaveFormatEx::parse(fmt_chunk.payload, &mut budget)?;
//! assert_eq!(wave_tags::codec_name(&fmt), Some("pcm_s16le"));
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! # Reference behaviour and its limits
//!
//! Where a table entry claims to be `ffprobe` 8.1's exact `codec_name`
//! spelling, it is backed by a probe command recorded next to it
//! ([`wave_tags`], [`video_tags`]) and repeated in
//! `docs/format/vaco-format-riff.md` so it can be re-derived when the pinned
//! reference version moves (plan 13 §1b). Tags this crate has not probed
//! resolve to `None` rather than a guessed spelling — see each table's docs
//! for what that excludes.

#![forbid(unsafe_code)]

pub mod bitmapinfo;
pub mod chunk;
pub mod info;
pub mod rf64;
pub mod video_tags;
pub mod wave;
pub mod wave_tags;

pub use bitmapinfo::{BitmapInfoHeader, Compression};
pub use chunk::{Chunk, ChunkHeader, ChunkId, ChunkIter, RiffHeader};
pub use rf64::{Ds64, Ds64TableEntry};
pub use wave::{WaveFormatEx, WaveFormatExtensible};
