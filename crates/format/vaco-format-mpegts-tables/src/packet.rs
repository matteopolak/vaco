//! The 188-byte transport packet and its adaptation field
//! (ISO/IEC 13818-1 §2.4.3.2 to §2.4.3.5).
//!
//! Everything here is a view over a caller-owned slice. Nothing allocates,
//! nothing reads, and every accessor is total: a malformed packet yields
//! `None` for the field that is malformed and leaves the rest readable, which
//! is what lets a demuxer recover a PID and a continuity counter from a packet
//! whose adaptation field is nonsense.

/// The sync byte every transport packet starts with.
pub const SYNC_BYTE: u8 = 0x47;

/// The plain transport packet size.
pub const TS_PACKET_SIZE: usize = 188;

/// Blu-ray M2TS: a 4-byte `TP_extra_header` precedes each 188-byte packet.
pub const M2TS_PACKET_SIZE: usize = 192;

/// 188 bytes plus 16 bytes of Reed-Solomon parity, as broadcast recorders
/// sometimes store. The parity is ignored; we do not FEC-correct.
pub const RS_PACKET_SIZE: usize = 204;

/// Stuffing packets carry this PID and no useful payload.
pub const NULL_PID: u16 = 0x1FFF;

/// PID of the Program Association Table.
pub const PAT_PID: u16 = 0x0000;
/// PID of the Conditional Access Table.
pub const CAT_PID: u16 = 0x0001;
/// PID of the Transport Stream Description Table.
pub const TSDT_PID: u16 = 0x0002;
/// DVB Network Information Table (EN 300 468 §5.1.3).
pub const NIT_PID: u16 = 0x0010;
/// DVB Service Description Table / Bouquet Association Table.
pub const SDT_PID: u16 = 0x0011;
/// DVB Event Information Table.
pub const EIT_PID: u16 = 0x0012;
/// DVB Running Status Table.
pub const RST_PID: u16 = 0x0013;
/// DVB Time and Date Table / Time Offset Table.
pub const TDT_PID: u16 = 0x0014;

/// Largest PID value the 13-bit field can hold.
pub const MAX_PID: u16 = 0x1FFF;

/// The PCR clock: 27 MHz, expressed as base × 300 + extension.
pub const PCR_HZ: i64 = 27_000_000;
/// The presentation clock the base field counts in.
pub const PTS_HZ: i64 = 90_000;
/// `PCR_HZ / PTS_HZ`. The extension field counts these.
pub const PCR_EXT_PER_TICK: i64 = 300;

/// Width of the PTS/DTS/PCR-base field. A recording longer than
/// `2^33 / 90000 ≈ 26.5` hours crosses the wrap, which broadcast recordings
/// routinely do.
pub const TS_WRAP_BITS: u32 = 33;

/// How the file stores its packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketStride {
    /// Plain 188-byte packets.
    Ts,
    /// Blu-ray M2TS: 4 leading bytes, then 188.
    M2ts,
    /// 188 bytes then 16 bytes of Reed-Solomon parity.
    Rs,
}

impl PacketStride {
    /// Every stride, in the order a probe must try them.
    ///
    /// 188 first because the tie must go to the plain form: a 192-byte file is
    /// also 188-byte-consistent at four of every 192 offsets, and a 204-byte
    /// file trivially contains 188-byte packets.
    pub const ALL: [Self; 3] = [Self::Ts, Self::M2ts, Self::Rs];

    /// The largest stride, so a caller can size one stack buffer that fits
    /// any of them.
    pub const MAX_STRIDE: usize = RS_PACKET_SIZE;

    /// Bytes from one sync byte to the next.
    #[must_use]
    pub const fn stride(self) -> usize {
        match self {
            Self::Ts => TS_PACKET_SIZE,
            Self::M2ts => M2TS_PACKET_SIZE,
            Self::Rs => RS_PACKET_SIZE,
        }
    }

    /// Bytes before the sync byte within one stride.
    #[must_use]
    pub const fn prefix(self) -> usize {
        match self {
            Self::M2ts => 4,
            Self::Ts | Self::Rs => 0,
        }
    }

    /// The value `-packetsize` would report.
    #[must_use]
    pub const fn declared_size(self) -> usize {
        self.stride()
    }

    /// The 188 bytes of transport packet inside one stride of `buf`.
    #[must_use]
    pub fn body(self, buf: &[u8]) -> Option<&[u8]> {
        buf.get(self.prefix()..self.prefix().checked_add(TS_PACKET_SIZE)?)
    }
}

/// The 4-byte transport packet header (§2.4.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TsHeader {
    /// `transport_error_indicator`: an upstream demodulator flagged this
    /// packet as containing at least one uncorrectable bit error.
    pub transport_error: bool,
    /// `payload_unit_start_indicator`: a PES packet or PSI section begins here.
    pub payload_unit_start: bool,
    pub transport_priority: bool,
    pub pid: u16,
    /// `transport_scrambling_control`: non-zero means CA-scrambled. We never
    /// descramble; the flag exists so the stream can still be *reported*.
    pub scrambling: u8,
    /// `adaptation_field_control`, verbatim. `0` is reserved and carries
    /// neither adaptation field nor payload.
    pub adaptation_control: u8,
    /// `continuity_counter`, incremented per packet *with payload* on a PID.
    pub continuity: u8,
}

impl TsHeader {
    /// Decode the four header bytes of a transport packet.
    ///
    /// Returns `None` only when the slice is too short or does not start with
    /// the sync byte — every bit pattern of the remaining three bytes is
    /// representable.
    #[must_use]
    pub fn parse(buf: &[u8]) -> Option<Self> {
        let b = buf.get(..4)?;
        let (b0, b1, b2, b3) = (*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?);
        if b0 != SYNC_BYTE {
            return None;
        }
        Some(Self {
            transport_error: b1 & 0x80 != 0,
            payload_unit_start: b1 & 0x40 != 0,
            transport_priority: b1 & 0x20 != 0,
            pid: (u16::from(b1 & 0x1F) << 8) | u16::from(b2),
            scrambling: (b3 >> 6) & 0x03,
            adaptation_control: (b3 >> 4) & 0x03,
            continuity: b3 & 0x0F,
        })
    }

    /// Whether an adaptation field is present (`adaptation_field_control` 2 or 3).
    #[must_use]
    pub const fn has_adaptation(&self) -> bool {
        self.adaptation_control & 0x02 != 0
    }

    /// Whether a payload is present (`adaptation_field_control` 1 or 3).
    #[must_use]
    pub const fn has_payload(&self) -> bool {
        self.adaptation_control & 0x01 != 0
    }

    /// Whether this is a stuffing packet.
    #[must_use]
    pub const fn is_null(&self) -> bool {
        self.pid == NULL_PID
    }

    /// Whether the payload is CA-scrambled and therefore not parseable.
    #[must_use]
    pub const fn is_scrambled(&self) -> bool {
        self.scrambling != 0
    }
}

/// A Program Clock Reference, held in both of the units it is defined in.
///
/// The 27 MHz value is authoritative: `base * 300 + extension` is exact and
/// the 90 kHz base alone loses the extension. Both are kept because the
/// presentation clock is 90 kHz and converting on every comparison is where
/// rounding creeps in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pcr {
    /// 33-bit `program_clock_reference_base`, counting 90 kHz ticks.
    pub base: i64,
    /// 9-bit `program_clock_reference_extension`, counting 27 MHz ticks
    /// within one 90 kHz tick.
    pub extension: u16,
}

impl Pcr {
    /// Decode the 48-bit field: 33 bits of base, 6 reserved, 9 of extension.
    #[must_use]
    pub fn parse(buf: &[u8]) -> Option<Self> {
        let b = buf.get(..6)?;
        let hi = u64::from(u32::from_be_bytes([
            *b.first()?,
            *b.get(1)?,
            *b.get(2)?,
            *b.get(3)?,
        ]));
        let b4 = *b.get(4)?;
        let b5 = *b.get(5)?;
        let base = (hi << 1) | u64::from(b4 >> 7);
        let extension = (u16::from(b4 & 0x01) << 8) | u16::from(b5);
        Some(Self {
            // 33 bits: the shift above produced exactly 33 significant bits.
            base: (base & ((1 << TS_WRAP_BITS) - 1)).cast_signed(),
            extension,
        })
    }

    /// The full 27 MHz value.
    #[must_use]
    pub const fn as_27mhz(&self) -> i64 {
        self.base
            .saturating_mul(PCR_EXT_PER_TICK)
            .saturating_add(self.extension as i64)
    }
}

/// The adaptation field (§2.4.3.4), decoded far enough to demux with.
///
/// `transport_private_data` and `adaptation_field_extension` are located but
/// not interpreted: nothing in a demuxer needs them, and parsing structures we
/// do not use is attack surface for no gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdaptationField {
    /// Total field length including the length byte itself.
    pub total_len: usize,
    /// A *legitimate* timestamp or continuity-counter jump follows. This is
    /// the flag that separates a splice from corruption.
    pub discontinuity: bool,
    /// The next packet on this PID starts a random access point.
    pub random_access: bool,
    pub es_priority: bool,
    pub pcr: Option<Pcr>,
    pub opcr: Option<Pcr>,
    /// `splice_countdown`, signed: packets remaining until a splice point.
    pub splice_countdown: Option<i8>,
}

impl AdaptationField {
    /// Parse the adaptation field at the start of `buf` (i.e. at byte 4 of the
    /// packet).
    ///
    /// A length byte of zero is legal and means "one stuffing byte, no flags",
    /// which is how a muxer pads a packet by exactly one byte.
    ///
    /// Returns `None` when the declared length runs past the packet, which is
    /// the malformed case the caller must treat as "no usable payload".
    #[must_use]
    pub fn parse(buf: &[u8]) -> Option<Self> {
        let len = usize::from(*buf.first()?);
        let total_len = len.checked_add(1)?;
        if total_len > buf.len() {
            return None;
        }
        let mut me = Self {
            total_len,
            ..Self::default()
        };
        if len == 0 {
            return Some(me);
        }
        let flags = *buf.get(1)?;
        me.discontinuity = flags & 0x80 != 0;
        me.random_access = flags & 0x40 != 0;
        me.es_priority = flags & 0x20 != 0;
        // The optional fields follow in a fixed order; each is skipped only if
        // its flag is clear, so a wrong flag misaligns everything after it —
        // which is why each read is bounded by the declared field, not by the
        // packet.
        let body = buf.get(2..total_len)?;
        let mut at = 0usize;
        if flags & 0x10 != 0 {
            me.pcr = body.get(at..).and_then(Pcr::parse);
            at = at.checked_add(6)?;
        }
        if flags & 0x08 != 0 {
            me.opcr = body.get(at..).and_then(Pcr::parse);
            at = at.checked_add(6)?;
        }
        if flags & 0x04 != 0 {
            me.splice_countdown = body.get(at).map(|&v| v.cast_signed());
        }
        Some(me)
    }
}

/// One transport packet, split into what a demuxer consumes.
#[derive(Debug, Clone, Copy)]
pub struct TsPacket<'a> {
    pub header: TsHeader,
    /// `None` when `adaptation_field_control` says there is none, or when the
    /// declared length was impossible.
    pub adaptation: Option<AdaptationField>,
    /// The payload bytes, empty rather than absent when the adaptation field
    /// consumed the whole packet.
    pub payload: &'a [u8],
    /// Set when the adaptation field declared a length past the end of the
    /// packet. The header is still trustworthy; the payload is not, and is
    /// reported empty.
    pub malformed_adaptation: bool,
}

impl<'a> TsPacket<'a> {
    /// Parse one 188-byte transport packet.
    ///
    /// `buf` must be the packet itself, not a stride: use
    /// [`PacketStride::body`] to strip an M2TS prefix first.
    #[must_use]
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        let header = TsHeader::parse(buf)?;
        let rest = buf.get(4..)?;
        let (adaptation, malformed, payload_at) = if header.has_adaptation() {
            match AdaptationField::parse(rest) {
                Some(af) => (Some(af), false, af.total_len),
                None => (None, true, rest.len()),
            }
        } else {
            (None, false, 0)
        };
        let payload = if header.has_payload() && !malformed {
            rest.get(payload_at..).unwrap_or(&[])
        } else {
            &[]
        };
        Some(Self {
            header,
            adaptation,
            payload,
            malformed_adaptation: malformed,
        })
    }

    /// The PCR this packet carries, if any.
    #[must_use]
    pub fn pcr(&self) -> Option<Pcr> {
        self.adaptation.and_then(|a| a.pcr)
    }

    /// Whether the adaptation field declares a legitimate discontinuity.
    #[must_use]
    pub fn discontinuity(&self) -> bool {
        self.adaptation.is_some_and(|a| a.discontinuity)
    }

    /// Whether the adaptation field marks a random access point.
    #[must_use]
    pub fn random_access(&self) -> bool {
        self.adaptation.is_some_and(|a| a.random_access)
    }
}

/// How many strided sync bytes `buf` holds starting at `at`, capped at `cap`.
///
/// The primitive both the probe and the resynchroniser are built from. It
/// counts *consecutive* hits and stops at the first miss, because a run is the
/// evidence and a total is not: 0x47 occurs in ordinary data about once every
/// 256 bytes, so a count over a large buffer says nothing.
#[must_use]
pub fn sync_run(buf: &[u8], at: usize, stride: PacketStride, cap: u32) -> u32 {
    let step = stride.stride();
    let mut pos = at.saturating_add(stride.prefix());
    let mut n = 0u32;
    while n < cap {
        match buf.get(pos) {
            Some(&SYNC_BYTE) => n = n.saturating_add(1),
            _ => break,
        }
        pos = match pos.checked_add(step) {
            Some(p) => p,
            None => break,
        };
    }
    n
}

/// The best `(stride, offset, run length)` in `buf`, or `None` if nothing
/// looks like a transport stream.
///
/// `max_offset` bounds how far in we look for the first sync byte, which is
/// what `resync_size` configures: a file that starts mid-packet is ordinary,
/// a file where the first sync byte is a megabyte in is not.
#[must_use]
pub fn find_stride(buf: &[u8], max_offset: usize, cap: u32) -> Option<(PacketStride, usize, u32)> {
    let mut best: Option<(PacketStride, usize, u32)> = None;
    let limit = max_offset.min(buf.len());
    for at in 0..=limit {
        for stride in PacketStride::ALL {
            let run = sync_run(buf, at, stride, cap);
            if run < 2 {
                continue;
            }
            // Strictly greater keeps the earliest offset and, within an
            // offset, the first stride in `ALL` order — so 188 wins a tie
            // against 192 and 204, which is what the tie-break rule requires.
            if best.is_none_or(|(_, _, b)| run > b) {
                best = Some((stride, at, run));
            }
        }
        if best.is_some_and(|(_, _, r)| r >= cap) {
            break;
        }
    }
    best
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn packet(pid: u16, pusi: bool, cc: u8, afc: u8) -> [u8; TS_PACKET_SIZE] {
        let mut p = [0xFFu8; TS_PACKET_SIZE];
        p[0] = SYNC_BYTE;
        p[1] = (u8::from(pusi) << 6) | ((pid >> 8) as u8 & 0x1F);
        p[2] = (pid & 0xFF) as u8;
        p[3] = (afc << 4) | (cc & 0x0F);
        p
    }

    #[test]
    fn header_fields_decode() {
        let p = packet(0x0100, true, 5, 1);
        let h = TsHeader::parse(&p).unwrap();
        assert_eq!(h.pid, 0x0100);
        assert!(h.payload_unit_start);
        assert_eq!(h.continuity, 5);
        assert!(h.has_payload());
        assert!(!h.has_adaptation());
    }

    #[test]
    fn a_missing_sync_byte_is_the_only_hard_failure() {
        let mut p = packet(0x100, false, 0, 1);
        p[0] = 0x46;
        assert!(TsHeader::parse(&p).is_none());
        assert!(TsPacket::parse(&p).is_none());
        assert!(TsHeader::parse(&[]).is_none());
    }

    #[test]
    fn null_packets_are_recognised() {
        let p = packet(NULL_PID, false, 0, 1);
        assert!(TsHeader::parse(&p).unwrap().is_null());
    }

    #[test]
    fn pcr_decodes_both_clocks() {
        // base = 1, extension = 3.
        let field = [0x00, 0x00, 0x00, 0x00, 0x80 | 0x7E, 0x03];
        let pcr = Pcr::parse(&field).unwrap();
        assert_eq!(pcr.base, 1);
        assert_eq!(pcr.extension, 3);
        assert_eq!(pcr.as_27mhz(), 303);
    }

    #[test]
    fn pcr_base_is_thirty_three_bits() {
        let field = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        let pcr = Pcr::parse(&field).unwrap();
        assert_eq!(pcr.base, (1i64 << 33) - 1);
        assert_eq!(pcr.extension, 0x1FF);
    }

    #[test]
    fn adaptation_field_of_length_zero_is_one_stuffing_byte() {
        let mut p = packet(0x100, false, 0, 3);
        p[4] = 0;
        let pkt = TsPacket::parse(&p).unwrap();
        assert_eq!(pkt.adaptation.unwrap().total_len, 1);
        assert_eq!(pkt.payload.len(), TS_PACKET_SIZE - 5);
    }

    #[test]
    fn adaptation_field_carrying_a_pcr() {
        let mut p = packet(0x100, false, 0, 3);
        p[4] = 7; // length
        p[5] = 0x10 | 0x80; // PCR present, discontinuity
        p[6..12].copy_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x7E, 0x00]);
        let pkt = TsPacket::parse(&p).unwrap();
        let af = pkt.adaptation.unwrap();
        assert!(af.discontinuity);
        assert_eq!(af.pcr.unwrap().base, 2);
        assert_eq!(pkt.payload.len(), TS_PACKET_SIZE - 4 - 8);
    }

    #[test]
    fn an_over_long_adaptation_field_yields_no_payload() {
        let mut p = packet(0x100, false, 0, 3);
        p[4] = 200;
        let pkt = TsPacket::parse(&p).unwrap();
        assert!(pkt.malformed_adaptation);
        assert!(pkt.payload.is_empty());
        assert_eq!(pkt.header.pid, 0x100);
    }

    #[test]
    fn adaptation_field_exactly_filling_the_packet() {
        let mut p = packet(0x100, false, 0, 3);
        p[4] = 183;
        p[5] = 0;
        let pkt = TsPacket::parse(&p).unwrap();
        assert!(!pkt.malformed_adaptation);
        assert!(pkt.payload.is_empty());
    }

    #[test]
    fn stride_detection_prefers_188() {
        let mut buf = vec![0u8; TS_PACKET_SIZE * 10];
        for i in 0..10 {
            buf[i * TS_PACKET_SIZE] = SYNC_BYTE;
        }
        let (stride, at, run) = find_stride(&buf, 1024, 10).unwrap();
        assert_eq!(stride, PacketStride::Ts);
        assert_eq!(at, 0);
        assert!(run >= 9);
    }

    #[test]
    fn stride_detection_finds_m2ts() {
        let mut buf = vec![0u8; M2TS_PACKET_SIZE * 12];
        for i in 0..12 {
            buf[i * M2TS_PACKET_SIZE + 4] = SYNC_BYTE;
        }
        let (stride, at, _) = find_stride(&buf, 1024, 10).unwrap();
        assert_eq!(stride, PacketStride::M2ts);
        assert_eq!(at, 0);
    }

    #[test]
    fn stride_detection_finds_a_mid_packet_start() {
        let mut buf = vec![0u8; 50 + TS_PACKET_SIZE * 10];
        for i in 0..10 {
            buf[50 + i * TS_PACKET_SIZE] = SYNC_BYTE;
        }
        let (_, at, _) = find_stride(&buf, 1024, 10).unwrap();
        assert_eq!(at, 50);
    }

    #[test]
    fn random_bytes_do_not_look_like_a_transport_stream() {
        let buf: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(31) >> 3) as u8)
            .collect();
        // A run of two is the floor; anything this finds must be short.
        if let Some((_, _, run)) = find_stride(&buf, 4096, 10) {
            assert!(run < 5, "false positive with run {run}");
        }
    }

    #[test]
    fn sync_run_never_reads_past_the_buffer() {
        let buf = [SYNC_BYTE; 8];
        assert_eq!(sync_run(&buf, 0, PacketStride::Ts, 10), 1);
        assert_eq!(sync_run(&buf, 9, PacketStride::Ts, 10), 0);
    }
}
