//! The MPEG-TS demuxer.
//!
//! Packet framing, PES assembly, continuity, the 33-bit clock, duration
//! estimation and seeking. The PSI/SI layer it stands on is
//! `vaco-format-mpegts-tables`.

use std::collections::VecDeque;

use vaco_codec_core::{AudioParameters, CodecParameters, VideoParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::options::FormatOptions;
use vaco_format_core::seek::{
    IndexEntry, PacketIndex, SeekFlags, SeekLanding, SeekTarget, binary_search,
};
use vaco_format_core::time::WrapState;
use vaco_format_core::{Demuxer, Disposition, ParserProvider, Program, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use vaco_format_mpegts_tables::descriptor::{
    Descriptor, TAG_ISO639_LANGUAGE, TAG_SUBTITLING, TAG_TELETEXT, TAG_VBI_TELETEXT,
};
use vaco_format_mpegts_tables::packet::{
    CAT_PID, MAX_PID, NULL_PID, PAT_PID, PacketStride, SDT_PID, TS_WRAP_BITS, TsPacket,
};
use vaco_format_mpegts_tables::psi::{Cat, Pat, Pmt, Sdt};
use vaco_format_mpegts_tables::section::{MAX_SECTION_LEN, Section, SectionAssembler};
use vaco_format_mpegts_tables::stream_type::resolve;
use vaco_format_mpegts_tables::{TIME_BASE, packet::SYNC_BYTE};

use crate::pes::PesHeader;

/// What MPEG-TS declares it can do.
///
/// `TS_DISCONT` is the load-bearing one: the adaptation field's
/// `discontinuity_indicator` marks a *legitimate* jump, so the monotonic-DTS
/// repair must not run and discontinuity policy belongs to the CLI.
///
/// `GENERIC_INDEX` because the container ships no index at all and the only
/// one that can ever exist is the one built from packets that went past.
pub const FLAGS: FormatFlags = FormatFlags::SHOW_IDS
    .union(FormatFlags::TS_DISCONT)
    .union(FormatFlags::GENERIC_INDEX)
    .union(FormatFlags::VARIABLE_FPS);

/// Largest PES packet assembled before the stream is treated as hostile.
///
/// A video PES packet with `PES_packet_length == 0` is terminated only by the
/// next one, so a stream that never starts another would otherwise accumulate
/// without limit. Six megabytes comfortably exceeds any real access unit —
/// a 4K intra frame at a sane bitrate is well under one — and it is charged to
/// the [`Budget`] besides, so a `Limits::strict` caller gets a smaller ceiling
/// still.
pub const MAX_PES_BYTES: usize = 6 << 20;

/// Most PIDs a section assembler is kept for.
///
/// Each costs a fixed 4 KiB, so this is a memory bound rather than a
/// correctness one: PAT, CAT, SDT and one PMT per program.
pub const MAX_PSI_PIDS: usize = 64;

/// How far into the file the first sync byte may be before we give up.
pub const MAX_RESYNC_BYTES: u64 = 1 << 20;

/// Bytes read looking for the initial PSI. Bounded so an input made entirely
/// of null packets terminates.
pub const MAX_HEADER_SCAN: u64 = 5 << 20;

/// Read-back distances for the tail duration scan, in bytes.
///
/// **Measured against ffprobe 8.1**, not guessed. A `.ts` file padded with
/// trailing null packets keeps its duration until the padding exceeds
/// 16,000,000 bytes and loses it beyond, and the boundary is sharp to within a
/// packet — so the reference's furthest read-back is exactly `250_000 << 6`.
/// `planning/18-formats.md` R15 says "up to three times", which is wrong by
/// four doublings.
pub const DURATION_READ_BACK: u64 = 250_000;
/// Number of doublings past [`DURATION_READ_BACK`]. Measured; see above.
pub const DURATION_MAX_RETRY: u32 = 6;

/// Per-PID PSI state.
#[derive(Debug)]
struct PsiPid {
    pid: u16,
    kind: PsiKind,
    asm: SectionAssembler,
    cc: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PsiKind {
    Pat,
    Cat,
    Sdt,
    Pmt,
}

/// Per-PID elementary stream state.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact about the PES packet in \
              progress, and collapsing them into a bitflags type would trade \
              a readable field name for a mask at every use site"
)]
#[derive(Debug)]
struct EsPid {
    pid: u16,
    stream_index: u32,
    /// Index into [`MpegTsDemuxer::clocks`].
    clock: usize,
    buf: Vec<u8>,
    /// `6 + PES_packet_length`, or `None` while unknown or unbounded.
    total: Option<usize>,
    started: bool,
    cc: Option<u8>,
    /// Byte offset of the transport packet that started this PES packet.
    pos: u64,
    /// A continuity gap or a transport error touched this PES packet.
    corrupt: bool,
    key: bool,
    /// A `discontinuity_indicator` applies to the next packet emitted.
    discontinuity: bool,
}

/// One program's clock. Wrap state is per program, never per stream (R7): a
/// multiplex shares one clock and correcting video while leaving audio alone
/// desynchronises them permanently.
#[derive(Debug)]
struct ProgramClock {
    pts: WrapState,
    dts: WrapState,
    last_pcr: Option<i64>,
    first_pcr: Option<i64>,
}

impl ProgramClock {
    fn new(opts: &FormatOptions) -> Self {
        Self {
            pts: WrapState::new(TS_WRAP_BITS).with_options(opts),
            dts: WrapState::new(TS_WRAP_BITS).with_options(opts),
            last_pcr: None,
            first_pcr: None,
        }
    }

    fn reset(&mut self) {
        self.pts.reset();
        self.dts.reset();
    }
}

/// What a scan learned about one stream's timeline.
#[derive(Debug, Clone, Copy, Default)]
struct ScanState {
    first_pts: Timestamp,
    last_pts: Timestamp,
    /// Smallest positive PTS increment seen. For video this *is* the frame
    /// duration — one PES packet carries one access unit — and it survives
    /// B-frame reordering, where consecutive deltas alternate but the smallest
    /// positive one is still one frame. For audio it is not: a PES packet
    /// holds a dozen frames, so the smallest PES-to-PES gap is a dozen frame
    /// durations and means nothing.
    min_delta: i64,
}

/// Counters a caller can read for triage. None of them changes behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DemuxStats {
    /// Transport packets whose `transport_error_indicator` was set.
    pub transport_errors: u64,
    /// Continuity-counter gaps, excluding declared discontinuities.
    pub continuity_gaps: u64,
    /// Packets skipped as exact duplicates of their predecessor.
    pub duplicates: u64,
    /// Declared discontinuities honoured.
    pub discontinuities: u64,
    /// Sections dropped for a failed CRC.
    pub crc_failures: u64,
    /// Bytes skipped resynchronising to a sync byte.
    pub resync_bytes: u64,
    /// Transport packets carrying a non-zero `transport_scrambling_control`.
    pub scrambled_packets: u64,
    /// PES packets abandoned for exceeding [`MAX_PES_BYTES`].
    pub oversized_pes: u64,
}

/// The MPEG-TS demuxer.
#[derive(Debug)]
pub struct MpegTsDemuxer {
    io: IoContext,
    opts: FormatOptions,
    stride: PacketStride,
    first_packet: u64,
    streams: Vec<Stream>,
    programs: Vec<Program>,
    metadata: Vec<(String, String)>,
    psi: Vec<PsiPid>,
    es: Vec<EsPid>,
    clocks: Vec<ProgramClock>,
    queue: VecDeque<Packet>,
    budget: Budget,
    index: PacketIndex,
    scan: Vec<ScanState>,
    duration: Option<Duration>,
    stats: DemuxStats,
    /// End of stream is sticky: `read_packet` consumes bytes before it can
    /// tell whether a packet follows, so without this the second call after
    /// the end reports the file's own tail as corruption.
    eof: bool,
    /// Suppresses emission while a scan is running.
    scanning: bool,
    pmt_pids: Vec<(u16, u16)>,
    /// Clock index per program, parallel to `programs`.
    program_clocks: Vec<usize>,
    transport_stream_id: Option<u16>,
}

impl MpegTsDemuxer {
    /// Open a transport stream.
    ///
    /// Reads a bounded prefix for PSI, then — when the source is seekable and
    /// `skip_estimate_duration_from_pts` is not set — a bounded prefix for
    /// start times and a bounded suffix for end times, because nothing in the
    /// container states either.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when no packet rhythm can be found,
    /// [`Error::LimitExceeded`] past `max_streams`, and whatever the transport
    /// reports.
    pub fn open(
        src: Box<dyn MediaSource>,
        _parsers: &dyn ParserProvider,
        opts: &FormatOptions,
    ) -> Result<Self> {
        Self::open_with_limits(src, opts, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling.
    ///
    /// `DemuxerDesc::open` takes no [`Limits`], so this is the constructor a
    /// caller that cares — an embedder, a fuzz target — has to reach for. See
    /// the docs file's signature-gap list.
    ///
    /// # Errors
    ///
    /// As [`MpegTsDemuxer::open`].
    pub fn open_with_limits(
        src: Box<dyn MediaSource>,
        opts: &FormatOptions,
        limits: Limits,
    ) -> Result<Self> {
        let io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let mut me = Self {
            io,
            opts: opts.clone(),
            stride: PacketStride::Ts,
            first_packet: 0,
            streams: Vec::new(),
            programs: Vec::new(),
            metadata: Vec::new(),
            psi: Vec::new(),
            es: Vec::new(),
            clocks: Vec::new(),
            queue: VecDeque::new(),
            budget: Budget::new(limits),
            index: PacketIndex::with_options(opts),
            scan: Vec::new(),
            duration: None,
            stats: DemuxStats::default(),
            eof: false,
            scanning: false,
            pmt_pids: Vec::new(),
            program_clocks: Vec::new(),
            transport_stream_id: None,
        };
        me.detect_stride()?;
        me.psi.push(PsiPid::new(PAT_PID, PsiKind::Pat));
        me.psi.push(PsiPid::new(CAT_PID, PsiKind::Cat));
        me.psi.push(PsiPid::new(SDT_PID, PsiKind::Sdt));
        // The synthetic program-zero clock, for streams belonging to no
        // program — which is every stream in a file whose PAT never arrives.
        me.clocks.push(ProgramClock::new(opts));
        me.read_header()?;
        Ok(me)
    }

    /// Counters for triage.
    #[must_use]
    pub const fn stats(&self) -> DemuxStats {
        self.stats
    }

    /// The packet stride detected in the file.
    #[must_use]
    pub const fn stride(&self) -> PacketStride {
        self.stride
    }

    /// The index built from packets seen so far.
    #[must_use]
    pub const fn index(&self) -> &PacketIndex {
        &self.index
    }

    /// First and last Program Clock Reference seen, in 27 MHz ticks.
    ///
    /// Exposed for diagnostics and for a future constant-bitrate seek: the PCR
    /// is the only position reference in the format that does not depend on
    /// having parsed a PES header.
    #[must_use]
    pub fn pcr_range(&self) -> Option<(i64, i64)> {
        let c = self.clocks.get(1).or_else(|| self.clocks.first())?;
        Some((c.first_pcr?, c.last_pcr?))
    }

    // ------------------------------------------------------------ framing

    /// Find the packet stride and the offset of the first sync byte.
    fn detect_stride(&mut self) -> Result<()> {
        let window = self
            .io
            .peek(1 << 16)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        let data = vaco_format_core::probe::ProbeData::new(&window);
        let Some((stride, at, _)) = crate::probe::best_run(&data) else {
            return Err(Error::InvalidData("no MPEG-TS packet rhythm found"));
        };
        self.stride = stride;
        self.first_packet = self.io.pos().saturating_add(at as u64);
        self.stats.resync_bytes = self.stats.resync_bytes.saturating_add(at as u64);
        self.io.seek(self.first_packet)?;
        Ok(())
    }

    /// Read one stride into `buf`, resynchronising if the sync byte is missing.
    ///
    /// Returns the byte offset the packet started at.
    fn next_stride(&mut self, buf: &mut [u8]) -> Result<u64> {
        let n = self.stride.stride();
        let Some(dst) = buf.get_mut(..n) else {
            return Err(Error::InvalidData("stride buffer too small"));
        };
        let mut pos = self.io.pos();
        self.io.read_exact(dst)?;
        if dst.get(self.stride.prefix()) == Some(&SYNC_BYTE) {
            return Ok(pos);
        }
        // Lost alignment. Scan forward a byte at a time for a sync byte, then
        // re-read a whole stride from there. Bounded, because a hostile file
        // is otherwise an unbounded scan.
        let mut skipped = 0u64;
        while skipped < MAX_RESYNC_BYTES {
            pos = self.io.pos();
            let b = self.io.r8()?;
            skipped = skipped.saturating_add(1);
            if b != SYNC_BYTE {
                continue;
            }
            let start = pos.saturating_sub(self.stride.prefix() as u64);
            self.io.seek(start)?;
            self.io.read_exact(dst)?;
            if dst.get(self.stride.prefix()) == Some(&SYNC_BYTE) {
                self.stats.resync_bytes = self.stats.resync_bytes.saturating_add(skipped);
                return Ok(start);
            }
            self.io.seek(pos.saturating_add(1))?;
        }
        Err(Error::InvalidData("lost transport packet synchronisation"))
    }

    // --------------------------------------------------------------- PSI

    fn psi_index(&self, pid: u16) -> Option<usize> {
        self.psi.iter().position(|p| p.pid == pid)
    }

    fn es_index(&self, pid: u16) -> Option<usize> {
        self.es.iter().position(|e| e.pid == pid)
    }

    /// Feed one PSI payload and act on whatever sections complete.
    fn handle_psi(&mut self, slot: usize, pusi: bool, payload: &[u8]) {
        // Sections are collected first so the assembler's borrow ends before
        // the tables are applied, which needs `&mut self`. A payload holds at
        // most twenty-three sections, each at most 4 KiB, so the vector is
        // bounded by the transport packet rather than by any declared length.
        let mut sections: Vec<Vec<u8>> = Vec::new();
        let Some(state) = self.psi.get_mut(slot) else {
            return;
        };
        let kind = state.kind;
        state.asm.push(pusi, payload, |raw| {
            if raw.len() <= MAX_SECTION_LEN {
                sections.push(raw.to_vec());
            }
        });
        for raw in sections {
            let Some(section) = Section::new(&raw) else {
                continue;
            };
            if section.header.syntax && !section.crc_ok() {
                self.stats.crc_failures = self.stats.crc_failures.saturating_add(1);
                continue;
            }
            match kind {
                PsiKind::Pat => self.apply_pat(&section),
                PsiKind::Pmt => self.apply_pmt(&section),
                PsiKind::Sdt => self.apply_sdt(&section),
                PsiKind::Cat => {
                    if let Some(cat) = Cat::parse(&section) {
                        // Recorded, not acted on: we do not descramble, and
                        // there is no legal or clean way to.
                        let _ = cat.descriptors().count();
                    }
                }
            }
        }
    }

    fn apply_pat(&mut self, section: &Section<'_>) {
        let Some(pat) = Pat::parse(section) else {
            return;
        };
        self.transport_stream_id = Some(pat.transport_stream_id);
        for entry in pat.entries() {
            if entry.program_number == 0 || entry.pid > MAX_PID {
                continue;
            }
            if self.pmt_pids.iter().any(|&(p, _)| p == entry.pid) {
                continue;
            }
            if self.psi.len() >= MAX_PSI_PIDS {
                break;
            }
            self.pmt_pids.push((entry.pid, entry.program_number));
            self.psi.push(PsiPid::new(entry.pid, PsiKind::Pmt));
        }
    }

    fn apply_pmt(&mut self, section: &Section<'_>) {
        let Some(pmt) = Pmt::parse(section) else {
            return;
        };
        let program_id = i64::from(pmt.program_number);
        let (prog, clock) = self.program_slot(program_id);
        // `Program` has no `pmt_pid`/`pcr_pid`/`pmt_version` fields, so the
        // three values `vaco-probe -show_programs` prints go in the metadata
        // list. See the docs file's signature-gap section.
        let pmt_pid = self
            .pmt_pids
            .iter()
            .find(|&&(_, n)| n == pmt.program_number)
            .map_or(0, |&(p, _)| p);
        if let Some(p) = self.programs.get_mut(prog) {
            set_meta(&mut p.metadata, "pmt_pid", pmt_pid.to_string());
            set_meta(&mut p.metadata, "pcr_pid", pmt.pcr_pid.to_string());
            set_meta(&mut p.metadata, "pmt_version", pmt.version.to_string());
        }

        let cap = usize::try_from(self.opts.max_streams).unwrap_or(usize::MAX);
        for entry in pmt.streams() {
            if entry.elementary_pid > MAX_PID || entry.elementary_pid == NULL_PID {
                continue;
            }
            if let Some(existing) = self.es_index(entry.elementary_pid) {
                // A PID already carrying a stream keeps it. Reassigning here
                // is what `merge_pmt_versions` is for and it is not
                // implemented; see the docs file.
                if let Some(e) = self.es.get(existing)
                    && let Some(p) = self.programs.get_mut(prog)
                    && !p.stream_indices.contains(&e.stream_index)
                {
                    p.stream_indices.push(e.stream_index);
                }
                continue;
            }
            if self.streams.len() >= cap {
                break;
            }
            self.add_stream(
                entry.stream_type,
                entry.elementary_pid,
                entry.descriptors,
                prog,
                clock,
            );
        }
    }

    fn add_stream(
        &mut self,
        stream_type: u8,
        pid: u16,
        descriptors: &[u8],
        program: usize,
        clock: usize,
    ) {
        let resolved = resolve(stream_type, descriptors);
        let index = self.streams.len() as u32;
        let media = resolved.codec.media_type();
        let mut params = CodecParameters::new(media);
        params.codec_tag = Some(resolved.codec_tag);
        if let Some(id) = resolved.codec.codec_id() {
            params.codec_id = Some(id);
        }
        match media {
            MediaType::Video => params.video = Some(VideoParameters::default()),
            MediaType::Audio => params.audio = Some(AudioParameters::default()),
            _ => {}
        }
        let mut stream = Stream {
            index,
            id: Some(i64::from(pid)),
            params,
            time_base: TIME_BASE,
            start_time: Timestamp::NONE,
            duration: None,
            frame_count: None,
            disposition: Disposition::empty(),
            metadata: Vec::new(),
        };
        // `TsCodec` carries codecs `CodecId` has no variant for; record the
        // name so nothing is lost when `codec_id` is `None`.
        stream.metadata_set("ts_codec", resolved.codec.name());
        apply_descriptors(&mut stream, descriptors);
        self.streams.push(stream);
        self.scan.push(ScanState::default());
        self.es.push(EsPid {
            pid,
            stream_index: index,
            clock,
            buf: Vec::new(),
            total: None,
            started: false,
            cc: None,
            pos: 0,
            corrupt: false,
            key: false,
            discontinuity: false,
        });
        if let Some(p) = self.programs.get_mut(program) {
            p.stream_indices.push(index);
        }
    }

    fn apply_sdt(&mut self, section: &Section<'_>) {
        let Some(sdt) = Sdt::parse(section) else {
            return;
        };
        if !sdt.actual {
            return;
        }
        for service in sdt.services() {
            let Some((provider, name)) = service.names() else {
                continue;
            };
            let id = i64::from(service.service_id);
            if let Some(p) = self.programs.iter_mut().find(|p| p.id == id) {
                set_meta(&mut p.metadata, "service_name", name);
                set_meta(&mut p.metadata, "service_provider", provider);
            }
        }
    }

    /// The program's slot and its clock, creating both together.
    ///
    /// They are created in one place deliberately: a clock index that drifts
    /// from its program index would correct one stream's wrap and not
    /// another's, which is exactly the desynchronisation R7 exists to prevent.
    fn program_slot(&mut self, program_id: i64) -> (usize, usize) {
        if let Some(i) = self.programs.iter().position(|p| p.id == program_id) {
            return (i, self.program_clocks.get(i).copied().unwrap_or(0));
        }
        self.programs.push(Program {
            id: program_id,
            stream_indices: Vec::new(),
            metadata: Vec::new(),
        });
        self.clocks.push(ProgramClock::new(&self.opts));
        let clock = self.clocks.len().saturating_sub(1);
        self.program_clocks.push(clock);
        (self.programs.len().saturating_sub(1), clock)
    }

    // ------------------------------------------------------------ pumping

    /// Read one transport packet and fold it in, queueing any PES packet it
    /// completes.
    fn pump(&mut self) -> Result<()> {
        let mut buf = [0u8; PacketStride::MAX_STRIDE];
        let pos = self.next_stride(&mut buf)?;
        let Some(body) = self.stride.body(&buf) else {
            return Err(Error::InvalidData("short transport packet"));
        };
        let Some(pkt) = TsPacket::parse(body) else {
            return Err(Error::InvalidData("transport packet lost its sync byte"));
        };
        if pkt.header.is_null() {
            return Ok(());
        }
        if pkt.header.transport_error {
            self.stats.transport_errors = self.stats.transport_errors.saturating_add(1);
        }
        if pkt.header.is_scrambled() {
            self.stats.scrambled_packets = self.stats.scrambled_packets.saturating_add(1);
        }

        // PCR is recorded for every program whose PCR PID this is. Cheap, and
        // it is the only wall-clock-free position reference the format has.
        if let Some(pcr) = pkt.pcr() {
            let v = pcr.as_27mhz();
            for clock in &mut self.clocks {
                if clock.first_pcr.is_none() {
                    clock.first_pcr = Some(v);
                }
                clock.last_pcr = Some(v);
            }
        }

        let pid = pkt.header.pid;
        let discontinuity = pkt.discontinuity();
        if discontinuity {
            self.stats.discontinuities = self.stats.discontinuities.saturating_add(1);
        }

        if let Some(slot) = self.psi_index(pid) {
            let payload = pkt.payload;
            let Some(state) = self.psi.get_mut(slot) else {
                return Ok(());
            };
            match check_continuity(
                &mut state.cc,
                pkt.header.continuity,
                pkt.header.has_payload(),
                discontinuity,
            ) {
                Continuity::Ok => {}
                Continuity::Duplicate => {
                    self.stats.duplicates = self.stats.duplicates.saturating_add(1);
                    return Ok(());
                }
                Continuity::Gap => {
                    state.asm.abandon();
                    self.stats.continuity_gaps = self.stats.continuity_gaps.saturating_add(1);
                }
            }
            if pkt.header.has_payload() && !pkt.header.is_scrambled() {
                self.handle_psi(slot, pkt.header.payload_unit_start, payload);
            }
            return Ok(());
        }

        let Some(slot) = self.es_index(pid) else {
            return Ok(());
        };
        self.handle_es(slot, &pkt, pos, discontinuity)
    }

    fn handle_es(
        &mut self,
        slot: usize,
        pkt: &TsPacket<'_>,
        pos: u64,
        discontinuity: bool,
    ) -> Result<()> {
        let mut flush_before = false;
        let mut corrupt_now = pkt.header.transport_error;
        {
            let Some(es) = self.es.get_mut(slot) else {
                return Ok(());
            };
            match check_continuity(
                &mut es.cc,
                pkt.header.continuity,
                pkt.header.has_payload(),
                discontinuity,
            ) {
                Continuity::Ok => {}
                Continuity::Duplicate => {
                    self.stats.duplicates = self.stats.duplicates.saturating_add(1);
                    return Ok(());
                }
                Continuity::Gap => {
                    self.stats.continuity_gaps = self.stats.continuity_gaps.saturating_add(1);
                    corrupt_now = true;
                }
            }
            if discontinuity {
                es.discontinuity = true;
            }
            if pkt.header.payload_unit_start && es.started && !es.buf.is_empty() {
                flush_before = true;
            }
        }
        if flush_before {
            self.flush_pes(slot)?;
        }
        let Some(es) = self.es.get_mut(slot) else {
            return Ok(());
        };
        if pkt.header.payload_unit_start {
            es.buf.clear();
            es.total = None;
            es.started = true;
            es.pos = pos;
            es.corrupt = false;
            es.key = pkt.random_access();
        }
        if corrupt_now {
            es.corrupt = true;
        }
        if !es.started || !pkt.header.has_payload() {
            return Ok(());
        }
        if pkt.header.is_scrambled() {
            // A scrambled payload cannot be framed; the stream is still
            // reported, which is what a user needs to see.
            es.corrupt = true;
            return Ok(());
        }
        let payload = pkt.payload;
        if es.buf.len().saturating_add(payload.len()) > MAX_PES_BYTES {
            let charged = es.buf.len() as u64;
            es.buf.clear();
            es.started = false;
            self.budget.release(charged);
            self.stats.oversized_pes = self.stats.oversized_pes.saturating_add(1);
            return Ok(());
        }
        self.budget.charge(payload.len() as u64)?;
        let Some(es) = self.es.get_mut(slot) else {
            return Ok(());
        };
        es.buf.extend_from_slice(payload);
        if es.total.is_none()
            && let Some(head) = es.buf.get(..6)
            && let (Some(&a), Some(&b)) = (head.get(4), head.get(5))
        {
            let declared = usize::from(u16::from_be_bytes([a, b]));
            es.total = (declared != 0).then(|| declared.saturating_add(6));
        }
        let complete = es.total.is_some_and(|t| es.buf.len() >= t);
        if complete {
            self.flush_pes(slot)?;
        }
        Ok(())
    }

    /// Turn the accumulated PES packet into a [`Packet`] and queue it.
    fn flush_pes(&mut self, slot: usize) -> Result<()> {
        let Some(es) = self.es.get_mut(slot) else {
            return Ok(());
        };
        let charged = es.buf.len() as u64;
        let (stream_index, clock, pos, corrupt, key, discont) = (
            es.stream_index,
            es.clock,
            es.pos,
            es.corrupt,
            es.key,
            core::mem::take(&mut es.discontinuity),
        );
        let raw = core::mem::take(&mut es.buf);
        es.total = None;
        es.started = false;
        self.budget.release(charged);
        let Some(header) = PesHeader::parse(&raw) else {
            return Ok(());
        };
        if header.is_padding() {
            return Ok(());
        }
        // A declared length shorter than what arrived means the muxer lied or
        // the stream was spliced; trust the declaration, which is what keeps a
        // corrupt packet from swallowing the next one's bytes.
        let end = header.total_len().unwrap_or(raw.len()).min(raw.len());
        let payload = raw.get(header.payload_offset..end).unwrap_or(&[]);
        if payload.is_empty() && header.pts.is_none() {
            return Ok(());
        }

        if discont {
            // A legitimate jump: the wrap tracker's delta history is about to
            // be meaningless, so drop it rather than let it invent a wrap.
            if let Some(c) = self.clocks.get_mut(clock) {
                c.reset();
            }
        }
        let (pts, dts) = {
            let Some(c) = self.clocks.get_mut(clock) else {
                return Ok(());
            };
            if let Some(v) = header.pts.ticks() {
                c.pts.observe(v);
            }
            if let Some(v) = header.dts.ticks() {
                c.dts.observe(v);
            }
            let pts = c.pts.correct(header.pts);
            // A stream with no DTS has PTS == DTS by definition; feeding the
            // PTS through the DTS tracker keeps the two clocks in step so a
            // stream that starts with PTS-only packets and later gains DTS
            // does not jump.
            let dts = if header.dts.is_some() {
                c.dts.correct(header.dts)
            } else {
                c.dts.correct(header.pts)
            };
            (pts, dts)
        };

        self.note_scan(stream_index, pts);
        if self.scanning {
            // A scan only needs the timeline, and a duration estimate reads up
            // to sixteen megabytes; allocating a packet per PES packet to
            // throw it away is the difference between a fast open and a slow
            // one.
            if let Some(v) = dts.ticks() {
                self.index.add(IndexEntry::keyframe(pos, Timestamp::new(v)));
            }
            return Ok(());
        }
        let mut pkt = Packet::from_slice(&mut self.budget, payload)?;
        pkt.stream_index = stream_index;
        pkt.pts = pts;
        pkt.dts = if header.dts.is_some() || header.pts.is_some() {
            dts
        } else {
            Timestamp::NONE
        };
        pkt.pos = Some(pos);
        let mut flags = PacketFlags::empty();
        let video = self
            .streams
            .get(stream_index as usize)
            .and_then(Stream::media_type)
            .is_some_and(|m| matches!(m, MediaType::Video));
        if key || !video {
            flags |= PacketFlags::KEY;
        }
        if corrupt {
            flags |= PacketFlags::CORRUPT;
        }
        pkt.flags = flags;

        if pkt.is_key()
            && let Some(v) = dts.ticks()
        {
            self.index.add(IndexEntry::keyframe(pos, Timestamp::new(v)));
        }
        self.queue.push_back(pkt);
        Ok(())
    }

    fn note_scan(&mut self, stream_index: u32, pts: Timestamp) {
        let Some(st) = usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.scan.get_mut(i))
        else {
            return;
        };
        let Some(v) = pts.ticks() else { return };
        if st.first_pts.is_none() || st.first_pts.ticks().is_some_and(|f| v < f) {
            st.first_pts = Timestamp::new(v);
        }
        if let Some(prev) = st.last_pts.ticks() {
            let delta = v.saturating_sub(prev);
            if delta > 0 && (st.min_delta == 0 || delta < st.min_delta) {
                st.min_delta = delta;
            }
        }
        if st.last_pts.ticks().is_none_or(|p| v > p) {
            st.last_pts = Timestamp::new(v);
        }
    }

    /// The presentation end of `index`'s timeline, as far as the scan can tell.
    ///
    /// **Measured against ffprobe 8.1.** The reference reports
    /// `last_packet.pts + last_packet.duration`, and that duration comes from
    /// the codec — the frame rate for video, `frame_size / sample_rate` for
    /// audio — which `find_stream_info` establishes and a demuxer with no
    /// parser cannot. For video the smallest inter-packet delta reproduces it
    /// exactly, because one video PES packet is one access unit. For audio
    /// nothing here can, so the last frame's own duration is left out and the
    /// answer is short by exactly one audio frame. See the docs file.
    fn end_pts(&self, index: usize) -> Option<i64> {
        let st = self.scan.get(index)?;
        let last = st.last_pts.ticks()?;
        let video = self
            .streams
            .get(index)
            .and_then(Stream::media_type)
            .is_some_and(|m| matches!(m, MediaType::Video));
        Some(if video {
            last.saturating_add(st.min_delta)
        } else {
            last
        })
    }

    // ------------------------------------------------------------ header

    /// Read PSI, then estimate the timeline the container does not state.
    fn read_header(&mut self) -> Result<()> {
        self.scanning = true;
        let start = self.io.pos();
        let probe_cap = u64::try_from(self.opts.probesize)
            .unwrap_or(MAX_HEADER_SCAN)
            .min(MAX_HEADER_SCAN);
        // Phase one: enough PSI for every PMT the PAT names, and enough
        // packets for every stream to show a first timestamp.
        loop {
            if self.io.pos().saturating_sub(start) >= probe_cap {
                break;
            }
            if self.header_complete() {
                break;
            }
            match self.pump() {
                Ok(()) => {}
                Err(Error::Eof | Error::UnexpectedEof) => break,
                Err(e) if e.is_recoverable() => {}
                Err(e) => return Err(e),
            }
        }
        for (stream, st) in self.streams.iter_mut().zip(self.scan.iter()) {
            stream.start_time = st.first_pts;
        }
        self.estimate_duration()?;
        self.rewind()?;
        self.scanning = false;
        Ok(())
    }

    /// Whether the header scan has learned everything it can.
    fn header_complete(&self) -> bool {
        !self.streams.is_empty()
            && self.pmt_pids.len() == self.programs.len()
            && self
                .scan
                .iter()
                .take(self.streams.len())
                .all(|s| s.first_pts.is_some())
    }

    /// The tail scan (R15), the only way an MPEG-TS file's length can be
    /// known.
    ///
    /// Read-back distances are measured against the reference; see
    /// [`DURATION_READ_BACK`].
    fn estimate_duration(&mut self) -> Result<()> {
        if self.opts.skip_estimate_duration_from_pts
            || self.io.seekability() == Seekability::None
            || self.streams.is_empty()
        {
            return Ok(());
        }
        let Some(size) = self.io.size() else {
            return Ok(());
        };
        let configured = u64::try_from(self.opts.duration_probesize).unwrap_or(0);
        let base = if configured > 0 {
            configured
        } else {
            DURATION_READ_BACK
        };
        for retry in 0..=DURATION_MAX_RETRY {
            let back = base.saturating_mul(1u64 << retry.min(40));
            let from = size.saturating_sub(back).max(self.first_packet);
            self.reset_stream_state();
            // The scan state has to go too. Keeping it would let the header
            // scan's end timestamps satisfy the retry condition below, and the
            // loop would report the end of the *probe window* as the end of
            // the file — which is precisely the failure a file with a long
            // tail of null packets produces.
            for st in &mut self.scan {
                *st = ScanState::default();
            }
            self.io.seek(from)?;
            loop {
                match self.pump() {
                    Ok(()) => {}
                    Err(Error::Eof | Error::UnexpectedEof) => break,
                    Err(e) if e.is_recoverable() => {}
                    Err(e) => return Err(e),
                }
            }
            self.flush_all()?;
            if self.scan.iter().all(|s| s.last_pts.is_some()) || from <= self.first_packet {
                break;
            }
        }
        let ends: Vec<Option<i64>> = (0..self.streams.len()).map(|i| self.end_pts(i)).collect();
        let mut latest: Option<i64> = None;
        let mut earliest: Option<i64> = None;
        for (i, stream) in self.streams.iter_mut().enumerate() {
            let (Some(first), Some(Some(end))) = (stream.start_time.ticks(), ends.get(i).copied())
            else {
                continue;
            };
            let ticks = end.saturating_sub(first).max(0);
            stream.duration = Timestamp::new(ticks).to_duration(TIME_BASE);
            latest = Some(latest.map_or(end, |v: i64| v.max(end)));
            earliest = Some(earliest.map_or(first, |v: i64| v.min(first)));
        }
        if let (Some(end), Some(start)) = (latest, earliest) {
            self.duration = Timestamp::new(end.saturating_sub(start).max(0)).to_duration(TIME_BASE);
        }
        Ok(())
    }

    /// Emit whatever every PID still holds. Used at end of input, where a
    /// video PES packet with no declared length is only complete because
    /// nothing follows it.
    fn flush_all(&mut self) -> Result<()> {
        for slot in 0..self.es.len() {
            let pending = self
                .es
                .get(slot)
                .is_some_and(|e| e.started && !e.buf.is_empty());
            if pending {
                self.flush_pes(slot)?;
            }
        }
        Ok(())
    }

    /// Forget every per-PID assembly state, keeping the stream list.
    fn reset_stream_state(&mut self) {
        for es in &mut self.es {
            self.budget.release(es.buf.len() as u64);
            es.buf.clear();
            es.total = None;
            es.started = false;
            es.cc = None;
            es.corrupt = false;
            es.discontinuity = false;
        }
        for p in &mut self.psi {
            p.asm.abandon();
            p.cc = None;
        }
        for c in &mut self.clocks {
            c.reset();
        }
        self.queue.clear();
    }

    fn rewind(&mut self) -> Result<()> {
        self.reset_stream_state();
        self.io.seek(self.first_packet)?;
        self.eof = false;
        Ok(())
    }

    // -------------------------------------------------------------- seek

    /// Scan forward from `pos` for the next PES packet carrying a DTS, and
    /// report where it started and what it said.
    ///
    /// The `read_timestamp` hook the frozen [`Demuxer`] trait does not have,
    /// kept as an inherent method so the demuxer can hand it to
    /// [`binary_search`] itself.
    fn probe_at(&mut self, pos: u64, limit: u64) -> Result<Option<(u64, Timestamp)>> {
        let from = pos.max(self.first_packet);
        if from >= limit {
            return Ok(None);
        }
        self.io.seek(from)?;
        let mut budget = limit.saturating_sub(from);
        let mut buf = [0u8; PacketStride::MAX_STRIDE];
        while budget > 0 {
            let Ok(at) = self.next_stride(&mut buf) else {
                return Ok(None);
            };
            budget = budget.saturating_sub(self.stride.stride() as u64);
            let Some(body) = self.stride.body(&buf) else {
                return Ok(None);
            };
            let Some(pkt) = TsPacket::parse(body) else {
                return Ok(None);
            };
            if !pkt.header.payload_unit_start || pkt.header.is_scrambled() {
                continue;
            }
            let Some(slot) = self.es_index(pkt.header.pid) else {
                continue;
            };
            let Some(header) = PesHeader::parse(pkt.payload) else {
                continue;
            };
            let ts = if header.dts.is_some() {
                header.dts
            } else {
                header.pts
            };
            if ts.is_none() {
                continue;
            }
            // Only the reference stream's timestamps answer the question, and
            // only a stream we know about is a reference stream.
            let _ = slot;
            return Ok(Some((at, ts)));
        }
        Ok(None)
    }

    fn seek_timestamp(&mut self, ts: Timestamp, flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        // The index first, when packets have already gone past.
        if !self.index.is_empty()
            && let Some(entry) = self.index.search(ts, flags)
        {
            return self.land(entry.pos, ts);
        }
        let hi = self.io.size().unwrap_or(u64::MAX);
        let lo = self.first_packet;
        // `SeekStrategy::choose` cannot be used here. It returns `Byte` for
        // any format declaring `TS_DISCONT`, because `FormatFlags` conflates
        // "timestamps may jump" with "byte position and time are unrelated".
        // MPEG-TS needs the first and not the second: a recording is
        // overwhelmingly monotonic and the reference bisects it. See the docs
        // file.
        let mut index = core::mem::take(&mut self.index);
        let landing: Result<Option<SeekLanding>> =
            binary_search(ts, lo, hi, &mut index, |p, l| self.probe_at(p, l));
        self.index = index;
        match landing? {
            Some(l) => self.land(l.pos, ts),
            None => self.land(self.first_packet, ts),
        }
    }

    fn land(&mut self, pos: u64, target: Timestamp) -> Result<()> {
        self.reset_stream_state();
        self.io.seek(pos)?;
        self.eof = false;
        // R10: a seek invalidates the cumulative wrap offset, because the new
        // position may be on the other side of a wrap. Recompute it from the
        // target and the first raw value seen after landing.
        if let Some((_, first)) = self.peek_timestamp()? {
            for c in &mut self.clocks {
                c.pts.resync(target, first);
                c.dts.resync(target, first);
            }
        }
        self.io.seek(pos)?;
        Ok(())
    }

    fn peek_timestamp(&mut self) -> Result<Option<(u64, Timestamp)>> {
        let at = self.io.pos();
        let limit = self.io.size().unwrap_or(u64::MAX);
        let found = self.probe_at(at, limit.min(at.saturating_add(1 << 20)))?;
        self.io.seek(at)?;
        Ok(found)
    }
}

impl PsiPid {
    const fn new(pid: u16, kind: PsiKind) -> Self {
        Self {
            pid,
            kind,
            asm: SectionAssembler::new(),
            cc: None,
        }
    }
}

/// The continuity check's three outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Continuity {
    Ok,
    /// The spec permits one exact repetition of a packet; its payload must be
    /// ignored rather than appended twice.
    Duplicate,
    Gap,
}

fn check_continuity(
    prev: &mut Option<u8>,
    cc: u8,
    has_payload: bool,
    discontinuity: bool,
) -> Continuity {
    if discontinuity {
        *prev = Some(cc);
        return Continuity::Ok;
    }
    let Some(last) = *prev else {
        *prev = Some(cc);
        return Continuity::Ok;
    };
    // The counter advances only on packets that carry a payload.
    let expected = if has_payload {
        last.wrapping_add(1) & 0x0F
    } else {
        last
    };
    if cc == expected {
        *prev = Some(cc);
        return Continuity::Ok;
    }
    if cc == last && has_payload {
        return Continuity::Duplicate;
    }
    *prev = Some(cc);
    Continuity::Gap
}

/// Set `key`, replacing in place so insertion order — which is output order —
/// survives.
fn set_meta(list: &mut Vec<(String, String)>, key: &str, value: impl Into<String>) {
    let value = value.into();
    if let Some(slot) = list.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value;
    } else {
        list.push((key.to_owned(), value));
    }
}

/// Fold the ES descriptor loop into the stream's language and disposition.
fn apply_descriptors(stream: &mut Stream, descriptors: &[u8]) {
    for d in vaco_format_mpegts_tables::DescriptorIter::new(descriptors) {
        match d.tag {
            TAG_ISO639_LANGUAGE => apply_language(stream, &d),
            TAG_TELETEXT | TAG_VBI_TELETEXT => apply_teletext(stream, &d),
            TAG_SUBTITLING => apply_subtitling(stream, &d),
            _ => {}
        }
    }
}

fn apply_language(stream: &mut Stream, d: &Descriptor<'_>) {
    let Some(entry) = d.iso639_languages().next() else {
        return;
    };
    if let Some(lang) = entry.as_str() {
        stream.metadata_set("language", lang);
    }
    // 13818-1 Table 2-60: 1 clean effects, 2 hearing impaired, 3 visual
    // impaired commentary.
    match entry.audio_type {
        2 => stream.disposition |= Disposition::HEARING_IMPAIRED,
        3 => stream.disposition |= Disposition::VISUAL_IMPAIRED,
        _ => {}
    }
}

fn apply_teletext(stream: &mut Stream, d: &Descriptor<'_>) {
    let Some(first) = d.teletext_pages().next() else {
        return;
    };
    if let Some(lang) = first.language_str() {
        stream.metadata_set("language", lang);
    }
    if first.is_hearing_impaired() {
        stream.disposition |= Disposition::HEARING_IMPAIRED;
    }
    stream.metadata_set("teletext_page", first.page().to_string());
    // One descriptor can declare several logical subtitle streams on one PID.
    // Splitting them into separate `Stream`s needs a PID-to-many-streams
    // mapping the demuxer does not have; the count is recorded so the gap is
    // visible rather than silent. See the docs file.
    let pages = d.teletext_pages().count();
    if pages > 1 {
        stream.metadata_set("teletext_pages", pages.to_string());
    }
}

fn apply_subtitling(stream: &mut Stream, d: &Descriptor<'_>) {
    let Some(first) = d.subtitling_entries().next() else {
        return;
    };
    if let Some(lang) = first.language_str() {
        stream.metadata_set("language", lang);
    }
    if first.is_hearing_impaired() {
        stream.disposition |= Disposition::HEARING_IMPAIRED;
    }
    let count = d.subtitling_entries().count();
    if count > 1 {
        stream.metadata_set("subtitle_streams", count.to_string());
    }
}

impl Demuxer for MpegTsDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn programs(&self) -> &[Program] {
        &self.programs
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }

    fn read_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some(p) = self.queue.pop_front() {
                return Ok(p);
            }
            if self.eof {
                return Err(Error::Eof);
            }
            match self.pump() {
                Ok(()) => {}
                Err(Error::Eof | Error::UnexpectedEof) => {
                    self.eof = true;
                    self.flush_all()?;
                    if self.queue.is_empty() {
                        return Err(Error::Eof);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let target = match target.stream_index() {
            Some(i) => {
                let st = usize::try_from(i)
                    .ok()
                    .and_then(|i| self.streams.get(i))
                    .ok_or(Error::InvalidData("seek names an unknown stream"))?;
                let rate = st
                    .params
                    .video
                    .as_ref()
                    .map_or(Rational::ZERO, |v| v.frame_rate);
                target.resolve_frames(rate, st.time_base)?
            }
            None => target,
        };
        match target {
            SeekTarget::Byte(pos) => {
                if self.io.seekability() == Seekability::None {
                    return Err(Error::NotSeekable);
                }
                self.reset_stream_state();
                let landing = self.probe_at(pos, self.io.size().unwrap_or(u64::MAX))?;
                let at = landing.map_or(pos.max(self.first_packet), |(p, _)| p);
                self.io.seek(at)?;
                self.eof = false;
                Ok(())
            }
            SeekTarget::Timestamp { ts, .. } => self.seek_timestamp(ts, flags),
            SeekTarget::Frame { .. } => Err(Error::Unsupported("unresolved frame seek")),
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }
}
