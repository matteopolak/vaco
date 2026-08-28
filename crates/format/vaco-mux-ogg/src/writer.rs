//! The Ogg muxer.
//!
//! Reuses [`vaco_demux_ogg::page`] for the wire shape of a page (capture
//! pattern, fixed-header layout, the checksum's zeroing convention) and
//! [`vaco_demux_ogg::crc`] for RFC 3533's CRC-32 — one definition of what a
//! page *is*, shared with the sibling demuxer per D19, the same pattern
//! `vaco-mux-flv` already uses against `vaco-demux-flv`.
//!
//! # Page boundaries are a policy choice, not a spec requirement
//!
//! RFC 3533 fixes what a page *is*; it does not fix how many packets go on
//! one. This muxer flushes a page once its body reaches
//! [`PREFERRED_PAGE_BODY`] or its segment table is full, and puts each
//! header packet on its own page. Real encoders make different choices —
//! `ffmpeg`'s own default groups a stream's non-identification header
//! packets onto one shared page — so a remux through this crate will not be
//! byte-identical to one through the reference even when both are perfectly
//! valid Ogg. D6 §0.3 already names this as the expected shape for
//! containers whose spec "permits a large space of valid files"; see
//! `docs/format/vaco-mux-ogg.md`.

use vaco_core::{Error, Rational, Result};
use vaco_demux_ogg::page::{self, OggHeaderFlags};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_limits::Limits;
use vaco_packet::Packet;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_format_core::Muxer;

use crate::headers;

/// Body bytes a page accumulates before this muxer flushes it on its own
/// initiative — a policy default, not a spec requirement; see the module
/// docs. 4 KiB keeps a page from growing unreasonably large for typical
/// compressed-audio packet sizes while not fragmenting every single packet
/// onto its own page either.
pub const PREFERRED_PAGE_BODY: usize = 4096;

/// Accumulates lacing values and body bytes for one not-yet-flushed page.
#[derive(Debug, Default)]
struct PageBuilder {
    segments: Vec<u8>,
    body: Vec<u8>,
}

impl PageBuilder {
    fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Push as much of `payload` as the segment table has room for.
    ///
    /// Returns the number of bytes consumed and whether the packet
    /// terminated within this call (a lacing value below
    /// [`page::CONTINUATION_VALUE`] was emitted). `consumed < payload.len()`
    /// with `terminated == false` means the segment table filled up
    /// mid-packet — the caller must flush this page and resume into a new
    /// one flagged [`OggHeaderFlags::CONTINUED`].
    fn push_packet(&mut self, payload: &[u8]) -> (usize, bool) {
        let mut consumed = 0usize;
        loop {
            if self.segments.len() >= page::MAX_SEGMENTS {
                return (consumed, false);
            }
            let remaining = payload.len().saturating_sub(consumed);
            if remaining >= usize::from(page::CONTINUATION_VALUE) {
                self.segments.push(page::CONTINUATION_VALUE);
                let Some(chunk) =
                    payload.get(consumed..consumed + usize::from(page::CONTINUATION_VALUE))
                else {
                    return (consumed, false);
                };
                self.body.extend_from_slice(chunk);
                consumed += usize::from(page::CONTINUATION_VALUE);
            } else {
                let Ok(last) = u8::try_from(remaining) else {
                    return (consumed, false);
                };
                self.segments.push(last);
                if let Some(chunk) = payload.get(consumed..) {
                    self.body.extend_from_slice(chunk);
                }
                return (payload.len(), true);
            }
        }
    }

    fn take(&mut self) -> (Vec<u8>, Vec<u8>) {
        (
            core::mem::take(&mut self.segments),
            core::mem::take(&mut self.body),
        )
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact about this stream's write \
              state, and collapsing them into a state machine would trade a \
              readable field name for a match at every use site — the same \
              call `vaco-demux-mpegts::EsPid` makes"
)]
#[derive(Debug)]
struct StreamState {
    serial: u32,
    time_base: Rational,
    header_packets: Vec<Vec<u8>>,
    sequence: u32,
    builder: PageBuilder,
    /// Total ticks (in `time_base`) decoded so far, updated whenever a
    /// packet terminates. This is the granule position directly — for every
    /// mapping in `vaco_demux_ogg::granule`, the raw granule field *is* a
    /// plain running total of decoded units. Opus's `pre_skip` shifts the
    /// *demuxer's* reported timestamp (RFC 7845 §4: `timestamp = granule -
    /// pre_skip`), but the granule field itself never subtracts it — a
    /// caller's first packet already reports `pre_skip` worth of real
    /// decoded samples, it is simply told not to *play* them. Adding
    /// `pre_skip` again here was tried and measured wrong: it produced
    /// `29112` against an expected `28800` for 30 packets of 960 samples,
    /// exactly one `pre_skip` (312) too many.
    granule_cursor: i64,
    /// Whether a packet has terminated in the *current, unflushed* page.
    /// Distinguishes "nothing finished here, stamp `-1`" from "the running
    /// total legitimately did not move".
    terminated_this_page: bool,
    /// Set when the page most recently flushed cut a packet off mid-stream;
    /// the *next* page flushed for this stream must then carry
    /// [`OggHeaderFlags::CONTINUED`] to say it resumes that packet.
    pending_continued: bool,
    header_written: bool,
    eos_written: bool,
    ever_flushed: bool,
}

/// The Ogg muxer. One implementation behind five registrations
/// (`vaco_mux_ogg::MUXER_OGG`/`OGA`/`OGV`/`OPUS`/`SPX`) differing only in
/// their declared default codecs and extensions.
#[derive(Debug)]
pub struct OggMuxer {
    out: IoWriter,
    streams: Vec<StreamState>,
    header_written: bool,
    trailer_written: bool,
}

impl OggMuxer {
    /// A muxer writing to `sink`.
    ///
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter::new`].
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(
                sink,
                &IoOptions::default().with_limits(Limits::permissive()),
            )?,
            streams: Vec::new(),
            header_written: false,
            trailer_written: false,
        })
    }

    fn stream_mut(&mut self, index: u32) -> Result<&mut StreamState> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.streams.get_mut(i))
            .ok_or(Error::InvalidData("packet names an unknown stream"))
    }

    /// Write one already-built page for `stream_index`, computing its CRC.
    fn write_page(
        &mut self,
        stream_index: usize,
        flags: OggHeaderFlags,
        granule: i64,
        segments: &[u8],
        body: &[u8],
    ) -> Result<()> {
        let Some(st) = self.streams.get_mut(stream_index) else {
            return Ok(());
        };
        let mut page = Vec::new();
        page.extend_from_slice(&page::CAPTURE_PATTERN);
        page.push(page::SUPPORTED_VERSION);
        page.push(flags.bits());
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&st.serial.to_le_bytes());
        page.extend_from_slice(&st.sequence.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        let Ok(n) = u8::try_from(segments.len()) else {
            return Err(Error::InvalidData("page has too many segments"));
        };
        page.push(n);
        page.extend_from_slice(segments);
        page.extend_from_slice(body);
        let crc = vaco_demux_ogg::crc::crc32(&page);
        if let Some(dst) = page.get_mut(page::CHECKSUM_OFFSET..page::CHECKSUM_OFFSET + 4) {
            dst.copy_from_slice(&crc.to_le_bytes());
        }
        st.sequence = st.sequence.wrapping_add(1);
        st.ever_flushed = true;
        self.out.write(&page)
    }

    /// Flush `stream_index`'s current page, if it has anything in it (or
    /// `force` says to send an empty one, used for a stream with no data at
    /// all when the trailer still needs to mark its `EOS`).
    fn flush_page(
        &mut self,
        stream_index: usize,
        eos: bool,
        continues_next: bool,
        force: bool,
    ) -> Result<()> {
        let Some(st) = self.streams.get_mut(stream_index) else {
            return Ok(());
        };
        if st.builder.is_empty() && !force {
            return Ok(());
        }
        let granule = if st.terminated_this_page {
            st.granule_cursor
        } else {
            page::GRANULE_UNSET
        };
        let mut flags = OggHeaderFlags::empty();
        if eos {
            flags |= OggHeaderFlags::EOS;
        }
        if st.pending_continued {
            flags |= OggHeaderFlags::CONTINUED;
        }
        let (segments, body) = st.builder.take();
        st.terminated_this_page = false;
        st.pending_continued = continues_next;
        self.write_page(stream_index, flags, granule, &segments, &body)?;
        Ok(())
    }
}

impl Muxer for OggMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "streams must be added before the header is written",
            ));
        }
        let serial = u32::try_from(self.streams.len())
            .map_err(|_| Error::InvalidData("too many streams"))?;
        let sample_rate_base = params
            .audio
            .as_ref()
            .filter(|a| a.sample_rate > 0)
            .and_then(|a| i32::try_from(a.sample_rate).ok())
            .map_or(Rational::new(1, 1), |rate| Rational::new(1, rate));
        let (time_base, header_packets) = match params.codec_id {
            Some(CodecId::Opus) => {
                let extradata = params
                    .extradata
                    .clone()
                    .ok_or(Error::InvalidData("Opus needs OpusHead extradata"))?;
                let headers = vec![extradata, headers::opus_tags()];
                (Rational::new(1, 48_000), headers)
            }
            Some(CodecId::Flac) => {
                let streaminfo = params
                    .extradata
                    .clone()
                    .ok_or(Error::InvalidData("FLAC needs STREAMINFO extradata"))?;
                let first = headers::flac_first_packet(&streaminfo)?;
                (sample_rate_base, vec![first, headers::flac_comment_block()])
            }
            // Vorbis's three header packets (identification, comment,
            // setup) arrive as one `extradata` blob, Xiph-packed the way
            // `vaco-demux-ogg::codec::pack_xiph_headers` measured against a
            // real `ffmpeg -c:a vorbis` file -- see that function's doc
            // comment for the exact byte layout. Unpacking with its own
            // inverse, rather than re-deriving the format here, is the same
            // "one definition of what a page is" reasoning this crate's own
            // Cargo.toml already gives for depending on vaco-demux-ogg at
            // all (D19). The setup header carries encoder-chosen codebooks
            // this crate has no way to synthesise, so a caller not
            // supplying all three (still the case before vaco-demux-ogg's
            // own fix landed, and for anyone building CodecParameters by
            // hand) is refused rather than handed a stream a decoder cannot
            // use -- silently writing the identification packet's bytes as
            // if they meant something else, or the whole packed blob as a
            // single mis-shapen packet, is worse than an error.
            Some(CodecId::Vorbis) => {
                let extradata = params.extradata.clone().ok_or(Error::InvalidData(
                    "Vorbis needs identification/comment/setup extradata",
                ))?;
                let headers = vaco_demux_ogg::codec::split_xiph_headers(&extradata)
                    .filter(|h| h.len() == 3)
                    .ok_or(Error::InvalidData(
                        "Vorbis extradata must be 3 Xiph-packed header packets",
                    ))?;
                (sample_rate_base, headers)
            }
            _ => {
                let headers = params.extradata.clone().map_or_else(Vec::new, |e| vec![e]);
                (sample_rate_base, headers)
            }
        };
        self.streams.push(StreamState {
            serial,
            time_base,
            header_packets,
            sequence: 0,
            builder: PageBuilder::default(),
            granule_cursor: 0,
            terminated_this_page: false,
            pending_continued: false,
            header_written: false,
            eos_written: false,
            ever_flushed: false,
        });
        Ok(serial)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("header written twice"));
        }
        self.header_written = true;
        for i in 0..self.streams.len() {
            let packets = self
                .streams
                .get(i)
                .map(|s| s.header_packets.clone())
                .unwrap_or_default();
            for (k, packet) in packets.iter().enumerate() {
                let Some(st) = self.streams.get_mut(i) else {
                    continue;
                };
                let (consumed, terminated) = st.builder.push_packet(packet);
                debug_assert!(consumed == packet.len() && terminated);
                let flags = if k == 0 {
                    OggHeaderFlags::BOS
                } else {
                    OggHeaderFlags::empty()
                };
                let (segments, body) = st.builder.take();
                self.write_page(i, flags, page::GRANULE_UNSET, &segments, &body)?;
            }
            if let Some(st) = self.streams.get_mut(i) {
                st.header_written = true;
            }
        }
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("packet written before the header"));
        }
        let index = usize::try_from(packet.stream_index)
            .map_err(|_| Error::InvalidData("stream index does not fit"))?;
        let time_base = self.stream_mut(packet.stream_index)?.time_base;
        let duration_ticks = packet.duration.to_ticks(time_base).unwrap_or(0).max(0);

        let mut payload = packet.payload();
        loop {
            let Some(st) = self.streams.get_mut(index) else {
                return Err(Error::InvalidData("packet names an unknown stream"));
            };
            let (consumed, terminated) = st.builder.push_packet(payload);
            if terminated {
                st.granule_cursor = st.granule_cursor.saturating_add(duration_ticks);
                st.terminated_this_page = true;
            }
            let full_body = st.builder.body.len() >= PREFERRED_PAGE_BODY;
            let table_full = st.builder.segments.len() >= page::MAX_SEGMENTS;
            let Some(rest) = payload.get(consumed..) else {
                break;
            };
            payload = rest;
            if !terminated || full_body || table_full {
                // Either the segment table is genuinely full (must flush to
                // make room), or a completed packet pushed the page over the
                // preferred size (flush as a natural boundary). Either way
                // the *next* page must announce a continuation only when the
                // packet itself did not finish.
                self.flush_page(index, false, !terminated, false)?;
            }
            if payload.is_empty() {
                break;
            }
        }
        Ok(())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.streams.get(i))
            .map(|s| s.time_base)
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("trailer written before the header"));
        }
        if self.trailer_written {
            return Err(Error::InvalidData("trailer written twice"));
        }
        self.trailer_written = true;
        for i in 0..self.streams.len() {
            let force = self.streams.get(i).is_some_and(|s| !s.ever_flushed);
            self.flush_page(i, true, false, force)?;
            if let Some(st) = self.streams.get_mut(i) {
                st.eos_written = true;
            }
        }
        self.out.flush()
    }
}
