//! The `nut` demuxer: startcode/packet framing, the frame-code table, and
//! per-stream timestamp reconstruction (lsb/full `coded_pts`, the
//! `decode_delay`-sized dts reorder buffer, syncpoint `last_pts` resets).
//!
//! # What is read, and what is structurally skipped
//!
//! `main_header` and every `stream_header` are fully decoded (they are
//! mandatory — nothing else can be demuxed without them). `syncpoint` is
//! decoded far enough to reset each stream's `last_pts` per spec, but its
//! `back_ptr` is stored and not yet used for backward seeking (this
//! demuxer reads sequentially). `info` and `index` packets are skipped by
//! their own `forward_ptr` — real metadata and index-based seeking are
//! deferred, not silently wrong: neither is claimed anywhere in this
//! crate. `reserved_headers` (an unknown startcode NUT's own extensibility
//! model allows) are skipped the same way, per spec ("demuxers MUST ignore
//! new and unknown headers").

use std::collections::VecDeque;

use crate::codecs::{audio_codec_from_fourcc, video_codec_from_fourcc};
use crate::header::{
    FLAG_CHECKSUM, FLAG_CODED, FLAG_CODED_PTS, FLAG_EOR, FLAG_HEADER_IDX, FLAG_INVALID, FLAG_KEY,
    FLAG_MATCH_TIME, FLAG_RESERVED, FLAG_SIZE_MSB, FLAG_STREAM_ID, MainHeader, StreamClassData,
    StreamHeader,
};
use crate::startcode::{
    FILE_ID_STRING, INDEX_STARTCODE, INFO_STARTCODE, MAIN_STARTCODE, STREAM_STARTCODE,
    SYNCPOINT_STARTCODE,
};
use crate::vlc::{IoFeed, convert_ts, read_s, read_t, read_v};
use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecParameters, VideoParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

pub const FLAGS: FormatFlags = FormatFlags::empty();

/// Longest a non-frame packet's payload may declare before this demuxer
/// treats the length as implausible. `forward_ptr` is attacker-controlled
/// (read straight off the wire before anything validates it), and this is
/// the bound checked *before* the `Budget`-backed allocation it would
/// otherwise size — 64 MiB is generous for a NUT header packet, which the
/// specification itself describes as "about 100 bytes" typical.
const MAX_HEADER_PACKET: u64 = 64 * 1024 * 1024;

/// Same bound, for one frame's `data_size`.
const MAX_FRAME_SIZE: u64 = 256 * 1024 * 1024;

struct StreamState {
    time_base_id: usize,
    msb_pts_shift: u32,
    decode_delay: usize,
    last_pts: i64,
    pts_cache: Vec<i64>,
    cache_primed: bool,
}

/// The `nut` demuxer.
pub struct NutDemuxer {
    io: IoContext,
    main: MainHeader,
    stream_headers: Vec<StreamHeader>,
    streams: Vec<Stream>,
    stream_state: Vec<StreamState>,
    budget: Budget,
    queue: VecDeque<Packet>,
    eof: bool,
}

impl std::fmt::Debug for NutDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NutDemuxer")
            .field("stream_count", &self.stream_headers.len())
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl NutDemuxer {
    /// Open a NUT file.
    ///
    /// # Errors
    /// [`Error::InvalidData`] for a bad signature or malformed headers;
    /// [`Error::Eof`] if the stream ends before any packet is produced.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        Self::open_with_limits(src, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling.
    ///
    /// # Errors
    /// As [`NutDemuxer::open`].
    pub fn open_with_limits(src: Box<dyn MediaSource>, limits: Limits) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let sig = io.peek(FILE_ID_STRING.len())?;
        if sig != FILE_ID_STRING {
            return Err(Error::InvalidData("nut: missing file_id_string"));
        }
        io.skip(FILE_ID_STRING.len() as u64)?;

        let mut demux = Self {
            io,
            main: MainHeader {
                version: 0,
                stream_count: 0,
                max_distance: 32768,
                time_bases: Vec::new(),
                frame_code_table: Vec::new(),
                elision_headers: vec![Vec::new()],
                main_flags: 0,
            },
            stream_headers: Vec::new(),
            streams: Vec::new(),
            stream_state: Vec::new(),
            budget: Budget::new(limits),
            queue: VecDeque::new(),
            eof: false,
        };
        while demux.queue.is_empty() && !demux.eof {
            demux.advance()?;
        }
        Ok(demux)
    }

    fn time_base(&self, id: usize) -> (u64, u64) {
        self.main.time_bases.get(id).copied().unwrap_or((1, 1))
    }

    /// Read and dispatch exactly one packet or frame.
    fn advance(&mut self) -> Result<()> {
        let first = self.io.peek(1)?;
        // `IoContext::peek` returns fewer bytes than asked only at true EOF
        // (its own doc comment), never an error — an empty slice here means
        // the file ends exactly on a packet/frame boundary. Missing this
        // check meant EOF's `None` failed the `!= Some(&b'N')` comparison
        // (a `None` is never `Some(_)`), routing straight into `read_frame`,
        // which then failed on its very first byte read with a hard
        // `UnexpectedEof` instead of the graceful `Eof` every real file
        // (which never ends mid-packet) actually needs.
        if first.is_empty() {
            return Err(Error::Eof);
        }
        if first.first() != Some(&b'N') {
            return self.read_frame();
        }
        self.read_startcoded_packet()
    }

    fn read_startcoded_packet(&mut self) -> Result<()> {
        let head = self.io.peek(16)?;
        if head.len() < 8 {
            self.eof = true;
            return Err(Error::Eof);
        }
        let startcode = u64::from_be_bytes(
            head.get(0..8)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::UnexpectedEof)?,
        );
        self.io.skip(8)?;
        let forward_ptr = read_v(&mut IoFeed(&mut self.io))?;
        if forward_ptr > MAX_HEADER_PACKET {
            return Err(Error::InvalidData(
                "nut: forward_ptr declares an implausible packet size",
            ));
        }
        if forward_ptr > 4096 {
            // header_checksum: not verified today (see module docs on
            // deferred integrity checking), but it must still be consumed.
            self.io.skip(4)?;
        }
        // `forward_ptr` spans the payload *and* the trailing footer
        // checksum (measured — see `vaco-hash::crc32_nut`'s docs for the
        // derivation): the checksum is the last 4 bytes of this span, not
        // 4 bytes after it.
        let payload_len = forward_ptr
            .checked_sub(4)
            .ok_or(Error::InvalidData("nut: forward_ptr too small to hold a checksum"))?;
        let mut payload = self.budget.alloc::<u8>(payload_len as usize)?;
        self.io.read_exact(&mut payload)?;
        // The footer checksum, likewise consumed and not verified.
        self.io.skip(4)?;

        if startcode == SYNCPOINT_STARTCODE {
            self.on_syncpoint(&payload)?;
        } else if startcode == MAIN_STARTCODE {
            self.main = MainHeader::parse(&payload, &mut self.budget)?;
        } else if startcode == STREAM_STARTCODE {
            self.on_stream_header(&payload)?;
        } else if startcode == INFO_STARTCODE || startcode == INDEX_STARTCODE {
            // Structurally skipped — see module docs.
        } else {
            // An unknown/reserved header: NUT's own forward-compatibility
            // mechanism. Ignored per spec, not an error.
        }
        Ok(())
    }

    fn on_stream_header(&mut self, payload: &[u8]) -> Result<()> {
        let sh = StreamHeader::parse(payload, &mut self.budget)?;
        let index = u32::try_from(self.stream_headers.len()).unwrap_or(u32::MAX);
        let time_base = self.time_base(sh.time_base_id as usize);
        let rational = Rational {
            num: i32::try_from(time_base.0).unwrap_or(1),
            den: i32::try_from(time_base.1).unwrap_or(1),
        };
        let media_type = match sh.stream_class {
            0 => MediaType::Video,
            1 => MediaType::Audio,
            2 => MediaType::Subtitle,
            _ => MediaType::Data,
        };
        let mut params = CodecParameters::new(media_type);
        match &sh.class_data {
            StreamClassData::Video { width, height, .. } => {
                let codec = video_codec_from_fourcc(&sh.fourcc);
                if let Some(c) = codec {
                    params = params.with_codec(c);
                }
                params.video = Some(VideoParameters {
                    width: u32::try_from(*width).unwrap_or(0),
                    height: u32::try_from(*height).unwrap_or(0),
                    coded_width: u32::try_from(*width).unwrap_or(0),
                    coded_height: u32::try_from(*height).unwrap_or(0),
                    ..VideoParameters::default()
                });
            }
            StreamClassData::Audio {
                samplerate_num,
                channel_count,
                ..
            } => {
                let codec = audio_codec_from_fourcc(&sh.fourcc);
                if let Some(c) = codec {
                    params = params.with_codec(c);
                }
                params.audio = Some(AudioParameters {
                    sample_rate: u32::try_from(*samplerate_num).unwrap_or(0),
                    layout: ChannelLayout::default_for(u32::try_from(*channel_count).unwrap_or(1)),
                    ..AudioParameters::default()
                });
            }
            StreamClassData::Other => {}
        }
        let mut stream = Stream::new(index, media_type, rational);
        stream.params = params;
        self.streams.push(stream);
        self.stream_state.push(StreamState {
            time_base_id: sh.time_base_id as usize,
            msb_pts_shift: u32::try_from(sh.msb_pts_shift).unwrap_or(0),
            decode_delay: usize::try_from(sh.decode_delay).unwrap_or(0),
            last_pts: 0,
            pts_cache: Vec::new(),
            cache_primed: false,
        });
        self.stream_headers.push(sh);
        Ok(())
    }

    fn on_syncpoint(&mut self, payload: &[u8]) -> Result<()> {
        let mut c = crate::vlc::Cursor::new(payload);
        let time_base_count = self.main.time_bases.len().max(1) as u64;
        let (ticks, id) = read_t(&mut c, time_base_count)?;
        let _back_ptr_div16 = read_v(&mut c)?; // stored nowhere yet; see module docs
        let sync_tb = self.time_base(id);
        for state in &mut self.stream_state {
            let stream_tb = self
                .main
                .time_bases
                .get(state.time_base_id)
                .copied()
                .unwrap_or((1, 1));
            let ticks_i64 = i64::try_from(ticks).unwrap_or(i64::MAX);
            state.last_pts = convert_ts(ticks_i64, sync_tb, stream_tb);
            state.cache_primed = false;
            state.pts_cache.clear();
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one frame header has this many independently-optional fields per spec; \
                  splitting it would scatter one coherent decode across several functions \
                  threading the same mutable state"
    )]
    fn read_frame(&mut self) -> Result<()> {
        let frame_code = self.io.r8()?;
        let entry = self
            .main
            .frame_code_table
            .get(usize::from(frame_code))
            .copied()
            .ok_or(Error::InvalidData(
                "nut: frame code table is not initialised",
            ))?;
        if entry.flags & FLAG_INVALID != 0 {
            return Err(Error::InvalidData("nut: frame uses a code marked invalid"));
        }
        let mut flags = entry.flags;
        let mut feed = IoFeed(&mut self.io);
        if flags & FLAG_CODED != 0 {
            let coded = u32::try_from(read_v(&mut feed)?).unwrap_or(0);
            flags ^= coded;
        }
        let stream_id = if flags & FLAG_STREAM_ID != 0 {
            read_v(&mut feed)?
        } else {
            entry.stream_id
        };
        let stream_idx = usize::try_from(stream_id)
            .map_err(|_| Error::InvalidData("nut: stream_id overflow"))?;
        if stream_idx >= self.stream_state.len() {
            return Err(Error::InvalidData("nut: frame names an unknown stream"));
        }

        let coded_pts = if flags & FLAG_CODED_PTS != 0 {
            Some(read_v(&mut feed)?)
        } else {
            None
        };
        let data_size_msb = if flags & FLAG_SIZE_MSB != 0 {
            read_v(&mut feed)?
        } else {
            0
        };
        let match_time_delta = if flags & FLAG_MATCH_TIME != 0 {
            read_s(&mut feed)?
        } else {
            entry.match_time_delta
        };
        let header_idx = if flags & FLAG_HEADER_IDX != 0 {
            read_v(&mut feed)?
        } else {
            entry.header_idx
        };
        let frame_res = if flags & FLAG_RESERVED != 0 {
            read_v(&mut feed)?
        } else {
            entry.reserved_count
        };
        for _ in 0..frame_res {
            read_v(&mut feed)?;
        }
        if flags & FLAG_CHECKSUM != 0 {
            // Not verified today; see module docs.
            self.io.skip(4)?;
        }

        let data_size = entry
            .data_size_lsb
            .saturating_add(data_size_msb.saturating_mul(entry.data_size_mul));
        if data_size > MAX_FRAME_SIZE {
            return Err(Error::InvalidData(
                "nut: frame declares an implausible size",
            ));
        }

        let state = self
            .stream_state
            .get_mut(stream_idx)
            .ok_or(Error::InvalidData("nut: stream_id overflow"))?;
        let pts = match coded_pts {
            Some(cp) => {
                let mask = (1u64 << state.msb_pts_shift).saturating_sub(1);
                if cp < (1u64 << state.msb_pts_shift) {
                    // lsb-relative reconstruction, exactly the spec's own
                    // `delta = last_pts - mask/2` formula (an intentional
                    // exact halving of a small bitmask, not a precision
                    // concern floats would improve).
                    let mask_i = i64::try_from(mask).unwrap_or(i64::MAX);
                    #[allow(
                        clippy::integer_division,
                        reason = "spec's own coded_pts reconstruction formula: mask/2"
                    )]
                    let delta = state.last_pts - mask_i / 2;
                    let cp_i = i64::try_from(cp).unwrap_or(0);
                    ((cp_i.wrapping_sub(delta)) & mask_i).wrapping_add(delta)
                } else {
                    let full = i64::try_from(cp).unwrap_or(i64::MAX);
                    full.saturating_sub(1i64 << state.msb_pts_shift)
                }
            }
            None => state.last_pts.saturating_add(entry.pts_delta),
        };
        state.last_pts = pts;

        // dts: a `decode_delay`-sized reorder buffer, exactly the
        // specification's own sample code (`get_dts`).
        if !state.cache_primed {
            state.pts_cache = vec![pts; state.decode_delay];
            state.cache_primed = true;
        }
        let dts = if state.decode_delay == 0 {
            pts
        } else {
            let mut out = pts;
            for slot in &mut state.pts_cache {
                if *slot < out {
                    std::mem::swap(slot, &mut out);
                }
            }
            out
        };

        let elision = self
            .main
            .elision_headers
            .get(usize::try_from(header_idx).unwrap_or(0))
            .cloned()
            .unwrap_or_default();
        let elide = data_size <= 4096 && !elision.is_empty();
        let stored_len = if elide {
            data_size.saturating_sub(elision.len() as u64)
        } else {
            data_size
        };
        let stored_len = usize::try_from(stored_len)
            .map_err(|_| Error::InvalidData("nut: frame size overflow"))?;

        let total_len = elision.len().saturating_add(stored_len);
        let mut pkt = Packet::alloc(&mut self.budget, total_len)?;
        {
            let buf = pkt.payload_mut();
            if let Some(head) = buf.get_mut(..elision.len()) {
                head.copy_from_slice(&elision);
            }
            if let Some(tail) = buf.get_mut(elision.len()..) {
                self.io.read_exact(tail)?;
            }
        }
        pkt.stream_index = u32::try_from(stream_idx).unwrap_or(0);
        pkt.pts = Timestamp::new(pts);
        pkt.dts = Timestamp::new(dts);
        if flags & FLAG_KEY != 0 {
            pkt.flags |= PacketFlags::KEY;
        }
        if flags & FLAG_EOR != 0 {
            pkt.flags |= PacketFlags::DISCARD;
        }
        let _ = match_time_delta;
        self.queue.push_back(pkt);
        Ok(())
    }
}

impl Demuxer for NutDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some(p) = self.queue.pop_front() {
                return Ok(p);
            }
            if self.eof {
                return Err(Error::Eof);
            }
            match self.advance() {
                Ok(()) => {}
                Err(Error::Eof) => self.eof = true,
                Err(e) => return Err(e),
            }
        }
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        // Index-based and back-pointer-based seeking are both deferred —
        // see the module docs. Sequential-only for now.
        Err(Error::NotSeekable)
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}
