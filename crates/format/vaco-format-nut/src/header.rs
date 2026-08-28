//! `main_header` and `stream_header`: the frame-code table construction
//! algorithm, time bases, elision headers, and the per-stream codec
//! declaration.
//!
//! # The frame-code table this crate writes
//!
//! A real `ffmpeg -f nut` file spreads size/timestamp/stream information
//! across a table tuned for compactness (measured: audio frames used
//! `data_size_mul=123` so a frame's low bits of size come for free from
//! *which* of ~123 near-identical codes was chosen). Reproducing that
//! packing scheme byte-for-byte would mean reverse-engineering an
//! unspecified muxer heuristic, not implementing the format — the spec
//! only requires *a* valid table, not a particular one. This crate's own
//! muxer writes the simplest table that is still fully spec-compliant:
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | `FLAG_INVALID` |
//! | 1 | Every real frame: `FLAG_CODED_PTS\|FLAG_STREAM_ID\|FLAG_SIZE_MSB\|FLAG_CODED`, `data_size_mul=1`, `data_size_lsb=0` — so `data_size` is exactly the transmitted `data_size_msb`, `stream_id`/`coded_pts` are always explicit, and `FLAG_CODED`'s `coded_flags` XOR toggles `FLAG_KEY`/`FLAG_CHECKSUM` per frame |
//! | 78 (`'N'`) | Auto-marked invalid by the construction algorithm itself (every table has this, whether the muxer asks for it or not) |
//! | 2..255 except 78 | `FLAG_INVALID` |
//!
//! Every field that could be compact (size, pts) is instead sent in full on
//! every frame. This costs a handful of extra bytes per frame and nothing
//! else — the decoder side (which a real file needs) is fully general and
//! reads whatever table is actually present, elision headers included.

use crate::vlc::{ByteFeed, Cursor, read_s, read_v, read_vb, write_s, write_v, write_vb};
use vaco_core::{Error, Result};
use vaco_limits::Budget;

pub const FLAG_KEY: u32 = 1 << 0;
pub const FLAG_EOR: u32 = 1 << 1;
pub const FLAG_CODED_PTS: u32 = 1 << 3;
pub const FLAG_STREAM_ID: u32 = 1 << 4;
pub const FLAG_SIZE_MSB: u32 = 1 << 5;
pub const FLAG_CHECKSUM: u32 = 1 << 6;
pub const FLAG_RESERVED: u32 = 1 << 7;
pub const FLAG_HEADER_IDX: u32 = 1 << 10;
pub const FLAG_MATCH_TIME: u32 = 1 << 11;
pub const FLAG_CODED: u32 = 1 << 12;
pub const FLAG_INVALID: u32 = 1 << 13;

/// The unspecified "muxer lacked sufficient information" sentinel for
/// `match_time_delta`.
pub const MATCH_TIME_UNSPECIFIED: i64 = 1 - (1i64 << 62);

/// One resolved entry of the 256-entry frame-code table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameCodeEntry {
    pub flags: u32,
    pub stream_id: u64,
    pub data_size_mul: u64,
    pub data_size_lsb: u64,
    pub pts_delta: i64,
    pub reserved_count: u64,
    pub match_time_delta: i64,
    pub header_idx: u64,
}

impl FrameCodeEntry {
    const fn invalid() -> Self {
        Self {
            flags: FLAG_INVALID,
            stream_id: 0,
            data_size_mul: 0,
            data_size_lsb: 0,
            pts_delta: 0,
            reserved_count: 0,
            match_time_delta: MATCH_TIME_UNSPECIFIED,
            header_idx: 0,
        }
    }
}

/// This crate's own muxer's usable code — see the module docs.
pub const GENERIC_FRAME_CODE: u8 = 1;
pub const GENERIC_FRAME_FLAGS: u32 = FLAG_CODED_PTS | FLAG_STREAM_ID | FLAG_SIZE_MSB | FLAG_CODED;

/// Parse the 256-entry frame-code table exactly per the specification's
/// construction algorithm (`main_header`'s `for(i=0;i<256;)` loop),
/// including the automatic `flags['N'] = FLAG_INVALID` this crate's own
/// muxer relies on but every demuxer must reproduce regardless of what a
/// muxer intended for that slot.
///
/// # Errors
/// Propagates VLC decode failures; [`Error::InvalidData`] if a batch's
/// `count` would run past 256 entries in a way the loop cannot terminate on
/// (defensive — the construction loop already bounds `i<256`, but a
/// batch declaring an enormous `count` should not spin).
pub fn read_frame_code_table(feed: &mut impl ByteFeed) -> Result<Vec<FrameCodeEntry>> {
    let mut table = vec![FrameCodeEntry::invalid(); 256];
    let mut tmp_pts: i64 = 0;
    let mut tmp_mul: u64 = 1;
    let mut tmp_stream: u64 = 0;
    let mut tmp_match: i64 = MATCH_TIME_UNSPECIFIED;
    let mut tmp_head_idx: u64 = 0;

    let mut i: usize = 0;
    let mut iterations = 0u32;
    while i < 256 {
        iterations += 1;
        if iterations > 512 {
            // Each batch fills at least one slot on every *successful*
            // iteration of the outer loop except when stalling on i=='N'
            // repeatedly is impossible (that only ever advances i once per
            // batch too) — 512 is a generous multiple of the 256 slots to
            // fill, bounding a pathological all-zero-count input.
            return Err(Error::InvalidData(
                "nut: frame-code table did not terminate",
            ));
        }
        let tmp_flag = u32::try_from(read_v(feed)?).unwrap_or(u32::MAX);
        let tmp_fields = read_v(feed)?;
        if tmp_fields > 0 {
            tmp_pts = read_s(feed)?;
        }
        if tmp_fields > 1 {
            tmp_mul = read_v(feed)?;
        }
        if tmp_fields > 2 {
            tmp_stream = read_v(feed)?;
        }
        let tmp_size = if tmp_fields > 3 { read_v(feed)? } else { 0 };
        let tmp_res = if tmp_fields > 4 { read_v(feed)? } else { 0 };
        let count = if tmp_fields > 5 {
            read_v(feed)?
        } else {
            tmp_mul.saturating_sub(tmp_size)
        };
        if tmp_fields > 6 {
            tmp_match = read_s(feed)?;
        }
        if tmp_fields > 7 {
            tmp_head_idx = read_v(feed)?;
        }
        for _ in 8..tmp_fields {
            read_v(feed)?; // tmp_reserved[i], discarded per spec
        }

        let mut j: u64 = 0;
        while j < count && i < 256 {
            if i == usize::from(b'N') {
                // Forced invalid; does not consume a `count` slot.
                i += 1;
                continue;
            }
            if let Some(slot) = table.get_mut(i) {
                *slot = FrameCodeEntry {
                    flags: tmp_flag,
                    stream_id: tmp_stream,
                    data_size_mul: tmp_mul,
                    data_size_lsb: tmp_size.saturating_add(j),
                    pts_delta: tmp_pts,
                    reserved_count: tmp_res,
                    match_time_delta: tmp_match,
                    header_idx: tmp_head_idx,
                };
            }
            i += 1;
            j += 1;
        }
    }
    Ok(table)
}

/// Write this crate's own three-batch table — see the module docs.
pub fn write_frame_code_table(out: &mut Vec<u8>) {
    // Batch 1: code 0, FLAG_INVALID, count=1 (default: tmp_mul(1)-tmp_size(0)).
    write_v(out, u64::from(FLAG_INVALID));
    write_v(out, 0); // tmp_fields=0

    // Batch 2: code 1, the generic code, count=1 (same defaults).
    write_v(out, u64::from(GENERIC_FRAME_FLAGS));
    write_v(out, 0);

    // Batch 3: codes 2..255 except 'N', FLAG_INVALID, count=253.
    write_v(out, u64::from(FLAG_INVALID));
    write_v(out, 6); // tmp_fields=6: pts,mul,stream,size,res,count
    write_s(out, 0); // tmp_pts
    write_v(out, 1); // tmp_mul
    write_v(out, 0); // tmp_stream
    write_v(out, 0); // tmp_size
    write_v(out, 0); // tmp_res
    write_v(out, 253); // count
}

/// `main_header`'s content (everything a demuxer needs before it can read
/// stream headers).
#[derive(Debug, Clone)]
pub struct MainHeader {
    pub version: u64,
    pub stream_count: u64,
    pub max_distance: u64,
    /// `(num, den)` pairs; `time_base[i] = num/den` seconds per tick.
    pub time_bases: Vec<(u64, u64)>,
    pub frame_code_table: Vec<FrameCodeEntry>,
    pub elision_headers: Vec<Vec<u8>>,
    pub main_flags: u64,
}

impl MainHeader {
    /// Parse from a packet's whole payload (already extracted via
    /// `forward_ptr` — see `demux.rs`).
    ///
    /// # Errors
    /// Propagates VLC/table decode failures.
    pub fn parse(payload: &[u8], budget: &mut Budget) -> Result<Self> {
        let mut c = Cursor::new(payload);
        let version = read_v(&mut c)?;
        let stream_count = read_v(&mut c)?;
        let max_distance = read_v(&mut c)?;
        let time_base_count = read_v(&mut c)?;
        if time_base_count == 0 {
            return Err(Error::InvalidData("nut: time_base_count MUST NOT be 0"));
        }
        // Bounded: this is a count of 2-field records read one at a time
        // below, not an allocation sized from an unchecked value — but cap
        // it generously anyway so a hostile count cannot force a huge
        // `Vec` before the reads that would naturally fail first.
        if time_base_count > 4096 {
            return Err(Error::InvalidData("nut: implausible time_base_count"));
        }
        let mut time_bases = Vec::new();
        for _ in 0..time_base_count {
            let num = read_v(&mut c)?;
            let den = read_v(&mut c)?;
            if num == 0 || den == 0 {
                return Err(Error::InvalidData("nut: time_base num/den MUST NOT be 0"));
            }
            time_bases.push((num, den));
        }
        let frame_code_table = read_frame_code_table(&mut c)?;
        let header_count_minus1 = read_v(&mut c)?;
        if header_count_minus1 >= 128 {
            return Err(Error::InvalidData("nut: header_count_minus1 MUST be <128"));
        }
        let mut elision_headers = vec![Vec::new()]; // elision_header[0] is fixed empty
        for _ in 0..header_count_minus1 {
            elision_headers.push(read_vb(&mut c, budget)?);
        }
        // Measured (D17), not per the written specification: the spec's
        // `main_header` unconditionally ends with `main_flags (v)` then
        // `reserved_bytes`. A real `ffmpeg -f nut` 8.1 file's own
        // `forward_ptr`/checksum framing proves its `main_header` content is
        // exactly as long as `elision_header[header_count_minus1]` finishes
        // consuming — there are 0 bytes left for `main_flags` at all, cross-
        // checked two independent ways (the next packet's startcode position
        // via `forward_ptr`, and `vaco_hash::crc32_nut` matching the trailing
        // checksum over exactly this content). `v` cannot encode a value in 0
        // bytes, so the reference encoder simply omits this field here; this
        // parser follows the reference binary rather than the spec text and
        // treats a `main_header` with nothing left after elision headers as
        // `main_flags = 0` (`BROADCAST_MODE` unset), instead of erroring.
        let main_flags = if c.remaining().is_empty() {
            0
        } else {
            read_v(&mut c)?
        };
        Ok(Self {
            version,
            stream_count,
            max_distance,
            time_bases,
            frame_code_table,
            elision_headers,
            main_flags,
        })
    }

    /// Serialise the packet *payload* (not including `packet_header`
    /// startcode/`forward_ptr` or the trailing checksum — see `mux.rs`,
    /// which wraps every non-frame packet identically).
    #[must_use]
    pub fn write(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_v(&mut out, self.version);
        write_v(&mut out, self.stream_count);
        write_v(&mut out, self.max_distance);
        write_v(&mut out, self.time_bases.len() as u64);
        for &(num, den) in &self.time_bases {
            write_v(&mut out, num);
            write_v(&mut out, den);
        }
        write_frame_code_table(&mut out);
        write_v(&mut out, 0); // header_count_minus1: no elision headers
        write_v(&mut out, self.main_flags);
        out
    }
}

/// `stream_class` values.
pub const STREAM_CLASS_VIDEO: u64 = 0;
pub const STREAM_CLASS_AUDIO: u64 = 1;
pub const STREAM_CLASS_SUBTITLE: u64 = 2;
pub const STREAM_CLASS_USERDATA: u64 = 3;

#[derive(Debug, Clone)]
pub enum StreamClassData {
    Video {
        width: u64,
        height: u64,
        sample_width: u64,
        sample_height: u64,
        colorspace_type: u64,
    },
    Audio {
        samplerate_num: u64,
        samplerate_denom: u64,
        channel_count: u64,
    },
    Other,
}

#[derive(Debug, Clone)]
pub struct StreamHeader {
    pub stream_id: u64,
    pub stream_class: u64,
    pub fourcc: Vec<u8>,
    pub time_base_id: u64,
    pub msb_pts_shift: u64,
    pub max_pts_distance: u64,
    pub decode_delay: u64,
    pub stream_flags: u64,
    pub codec_specific_data: Vec<u8>,
    pub class_data: StreamClassData,
}

impl StreamHeader {
    /// # Errors
    /// Propagates VLC decode failures.
    pub fn parse(payload: &[u8], budget: &mut Budget) -> Result<Self> {
        let mut c = Cursor::new(payload);
        let stream_id = read_v(&mut c)?;
        let stream_class = read_v(&mut c)?;
        let fourcc = read_vb(&mut c, budget)?;
        let time_base_id = read_v(&mut c)?;
        let msb_pts_shift = read_v(&mut c)?;
        if msb_pts_shift >= 16 {
            return Err(Error::InvalidData("nut: msb_pts_shift MUST be <16"));
        }
        let max_pts_distance = read_v(&mut c)?;
        let decode_delay = read_v(&mut c)?;
        // Not a spec-mandated ceiling (the spec's own text is only the
        // semantic "MUST NOT be set higher than necessary for a codec" —
        // H.264 B-pyramid uses 2, and the spec doesn't know of any codec
        // needing more than a handful) — a defensive cap of this crate's
        // own choosing. Fuzzing found this field unbounded and reaching
        // `demux.rs`'s `vec![pts; decode_delay]` reorder-buffer allocation
        // directly, bypassing `Budget` entirely: a `decode_delay` near
        // `u64::MAX` triggered a `malloc(109968141936)` attempt (measured,
        // real crash) rather than any real reordering. 256 is far beyond
        // any real codec's need while still catching that class of input.
        if decode_delay > 256 {
            return Err(Error::InvalidData(
                "nut: decode_delay is implausibly large",
            ));
        }
        let stream_flags = read_v(&mut c)?;
        let codec_specific_data = read_vb(&mut c, budget)?;
        let class_data = match stream_class {
            STREAM_CLASS_VIDEO => StreamClassData::Video {
                width: read_v(&mut c)?,
                height: read_v(&mut c)?,
                sample_width: read_v(&mut c)?,
                sample_height: read_v(&mut c)?,
                colorspace_type: read_v(&mut c)?,
            },
            STREAM_CLASS_AUDIO => StreamClassData::Audio {
                samplerate_num: read_v(&mut c)?,
                samplerate_denom: read_v(&mut c)?,
                channel_count: read_v(&mut c)?,
            },
            _ => StreamClassData::Other,
        };
        Ok(Self {
            stream_id,
            stream_class,
            fourcc,
            time_base_id,
            msb_pts_shift,
            max_pts_distance,
            decode_delay,
            stream_flags,
            codec_specific_data,
            class_data,
        })
    }

    #[must_use]
    pub fn write(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_v(&mut out, self.stream_id);
        write_v(&mut out, self.stream_class);
        write_vb(&mut out, &self.fourcc);
        write_v(&mut out, self.time_base_id);
        write_v(&mut out, self.msb_pts_shift);
        write_v(&mut out, self.max_pts_distance);
        write_v(&mut out, self.decode_delay);
        write_v(&mut out, self.stream_flags);
        write_vb(&mut out, &self.codec_specific_data);
        match &self.class_data {
            StreamClassData::Video {
                width,
                height,
                sample_width,
                sample_height,
                colorspace_type,
            } => {
                write_v(&mut out, *width);
                write_v(&mut out, *height);
                write_v(&mut out, *sample_width);
                write_v(&mut out, *sample_height);
                write_v(&mut out, *colorspace_type);
            }
            StreamClassData::Audio {
                samplerate_num,
                samplerate_denom,
                channel_count,
            } => {
                write_v(&mut out, *samplerate_num);
                write_v(&mut out, *samplerate_denom);
                write_v(&mut out, *channel_count);
            }
            StreamClassData::Other => {}
        }
        out
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn the_written_table_round_trips_through_the_general_reader() {
        let mut out = Vec::new();
        write_frame_code_table(&mut out);
        let mut c = Cursor::new(&out);
        let table = read_frame_code_table(&mut c).unwrap();
        assert_eq!(table.len(), 256);
        assert_eq!(table[0].flags, FLAG_INVALID);
        assert_eq!(
            table[usize::from(GENERIC_FRAME_CODE)].flags,
            GENERIC_FRAME_FLAGS
        );
        assert_eq!(table[usize::from(GENERIC_FRAME_CODE)].data_size_mul, 1);
        assert_eq!(table[usize::from(GENERIC_FRAME_CODE)].data_size_lsb, 0);
        assert_eq!(
            table[usize::from(b'N')].flags,
            FLAG_INVALID,
            "'N' is always forced invalid"
        );
        assert_eq!(table[255].flags, FLAG_INVALID);
        // Every other slot besides 0, 1 and 'N' should also be invalid.
        for (i, entry) in table.iter().enumerate() {
            if i == 0 || i == usize::from(GENERIC_FRAME_CODE) {
                continue;
            }
            assert_eq!(entry.flags, FLAG_INVALID, "slot {i} should be invalid");
        }
    }

    #[test]
    fn a_main_header_round_trips() {
        let mut table_bytes = Vec::new();
        write_frame_code_table(&mut table_bytes);
        let frame_code_table = read_frame_code_table(&mut Cursor::new(&table_bytes)).unwrap();
        let h = MainHeader {
            version: 3,
            stream_count: 2,
            max_distance: 32768,
            time_bases: vec![(1, 25), (1, 48000)],
            frame_code_table,
            elision_headers: vec![Vec::new()],
            main_flags: 0,
        };
        let bytes = h.write();
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let parsed = MainHeader::parse(&bytes, &mut budget).unwrap();
        assert_eq!(parsed.version, 3);
        assert_eq!(parsed.stream_count, 2);
        assert_eq!(parsed.time_bases, vec![(1, 25), (1, 48000)]);
        assert_eq!(parsed.frame_code_table[1].flags, GENERIC_FRAME_FLAGS);
    }

    #[test]
    fn a_video_stream_header_round_trips() {
        let h = StreamHeader {
            stream_id: 0,
            stream_class: STREAM_CLASS_VIDEO,
            fourcc: b"FMP4".to_vec(),
            time_base_id: 0,
            msb_pts_shift: 0,
            max_pts_distance: 25,
            decode_delay: 0,
            stream_flags: 0,
            codec_specific_data: vec![0x00, 0x00, 0x01, 0xb0],
            class_data: StreamClassData::Video {
                width: 64,
                height: 64,
                sample_width: 1,
                sample_height: 1,
                colorspace_type: 0,
            },
        };
        let bytes = h.write();
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let parsed = StreamHeader::parse(&bytes, &mut budget).unwrap();
        assert_eq!(parsed.fourcc, b"FMP4");
        match parsed.class_data {
            StreamClassData::Video { width, height, .. } => {
                assert_eq!(width, 64);
                assert_eq!(height, 64);
            }
            StreamClassData::Audio { .. } | StreamClassData::Other => panic!("wrong class"),
        }
    }
}
