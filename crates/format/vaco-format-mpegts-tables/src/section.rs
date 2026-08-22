//! PSI/SI section framing (ISO/IEC 13818-1 §2.4.4.10 and §2.4.4.11).
//!
//! A section is a byte string that may begin anywhere inside a transport
//! packet's payload, may span any number of packets, and may share a packet
//! with the tail of the previous one and the head of the next. The
//! `pointer_field` on a `payload_unit_start_indicator` packet says where the
//! *new* section begins; everything before it belongs to the section already in
//! progress.
//!
//! # Why the assembler owns a fixed array
//!
//! `section_length` is twelve bits, so a section can never exceed
//! `3 + 4093 = 4096` bytes. That is small enough to hold outright, which
//! removes the whole attacker-controlled-allocation question from the section
//! layer: there is no length field here that can make anything allocate. The
//! caller bounds how many PIDs get an assembler; each one costs a known 4 KiB.
//!
//! # What it deliberately does not do
//!
//! It does not check the CRC and it does not know what a table is. A caller
//! that wants only valid sections filters with
//! [`crate::crc::section_crc_ok`] — kept separate because a demuxer's
//! `err_detect` setting decides whether a bad CRC is a warning or an error,
//! and that is policy the framing layer must not bake in.

use crate::crc::section_crc_ok;

/// `3 + 4093`: the largest a twelve-bit `section_length` can describe.
pub const MAX_SECTION_LEN: usize = 4096;

/// Bytes before `section_length`'s own field ends.
const HEADER_MIN: usize = 3;

/// The cap `section_syntax_indicator == 1` tables obey: the two most
/// significant bits of `section_length` are reserved-zero for them, so a PAT,
/// PMT or CAT longer than this is malformed however plausible its CRC.
pub const MAX_PSI_SECTION_LEN: usize = 3 + 1021;

/// Stuffing after the last section in a packet.
pub const STUFFING_BYTE: u8 = 0xFF;

/// The common section header (§2.4.4.11).
///
/// `syntax` selects between the short form (three bytes, no CRC — TDT and
/// TOT) and the long form every table a demuxer cares about uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionHeader {
    pub table_id: u8,
    /// `section_syntax_indicator`. When false, only the three-byte header
    /// exists and the remaining fields below are meaningless.
    pub syntax: bool,
    /// `private_indicator` for private sections; the reserved `'0'` bit
    /// otherwise.
    pub private: bool,
    /// Bytes following the `section_length` field, so the whole section is
    /// `3 + section_length`.
    pub section_length: usize,
    /// `transport_stream_id` for a PAT, `program_number` for a PMT,
    /// `service_id` scope for an SDT.
    pub table_id_extension: u16,
    pub version: u8,
    /// When false, this table describes the *next* configuration and must not
    /// be applied yet.
    pub current_next: bool,
    pub section_number: u8,
    pub last_section_number: u8,
}

impl SectionHeader {
    /// Decode the header of `section`.
    ///
    /// Returns `None` for anything shorter than three bytes, or shorter than
    /// eight when the syntax indicator claims the long form.
    #[must_use]
    pub fn parse(section: &[u8]) -> Option<Self> {
        let table_id = *section.first()?;
        let b1 = *section.get(1)?;
        let b2 = *section.get(2)?;
        let syntax = b1 & 0x80 != 0;
        let section_length = (usize::from(b1 & 0x0F) << 8) | usize::from(b2);
        let mut me = Self {
            table_id,
            syntax,
            private: b1 & 0x40 != 0,
            section_length,
            table_id_extension: 0,
            version: 0,
            current_next: true,
            section_number: 0,
            last_section_number: 0,
        };
        if !syntax {
            return Some(me);
        }
        me.table_id_extension = u16::from_be_bytes([*section.get(3)?, *section.get(4)?]);
        let b5 = *section.get(5)?;
        me.version = (b5 >> 1) & 0x1F;
        me.current_next = b5 & 0x01 != 0;
        me.section_number = *section.get(6)?;
        me.last_section_number = *section.get(7)?;
        Some(me)
    }

    /// Total section size, header included.
    #[must_use]
    pub const fn total_len(&self) -> usize {
        HEADER_MIN.saturating_add(self.section_length)
    }
}

/// A complete section, with its header already decoded.
#[derive(Debug, Clone, Copy)]
pub struct Section<'a> {
    pub header: SectionHeader,
    /// The whole section including header and CRC.
    pub raw: &'a [u8],
}

impl<'a> Section<'a> {
    /// Wrap a complete section.
    #[must_use]
    pub fn new(raw: &'a [u8]) -> Option<Self> {
        Some(Self {
            header: SectionHeader::parse(raw)?,
            raw,
        })
    }

    /// The table body: everything after the eight-byte long-form header and
    /// before the four CRC bytes.
    ///
    /// `None` for a short-form section, which has no body in this sense, and
    /// for a section too short to hold both.
    #[must_use]
    pub fn body(&self) -> Option<&'a [u8]> {
        if !self.header.syntax {
            return None;
        }
        let end = self.raw.len().checked_sub(4)?;
        self.raw.get(8..end)
    }

    /// Whether the trailing CRC-32 checks out.
    ///
    /// A short-form section carries no CRC and is reported valid.
    #[must_use]
    pub fn crc_ok(&self) -> bool {
        !self.header.syntax || section_crc_ok(self.raw)
    }

    /// Whether this section is one a demuxer should act on: long form, CRC
    /// valid, and `current_next_indicator` set.
    #[must_use]
    pub fn is_applicable(&self) -> bool {
        self.header.syntax && self.header.current_next && self.crc_ok()
    }
}

/// Why an assembler threw away bytes. Counted, never fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AssemblerStats {
    /// A `pointer_field` pointing past the end of its own payload.
    pub bad_pointer: u64,
    /// A section declared longer than twelve bits can describe. Impossible by
    /// construction, so this counter should stay at zero; it exists because a
    /// non-zero value means the *framing* is wrong, not the file.
    pub over_long: u64,
    /// Bytes dropped because no section had started yet — the ordinary state
    /// when a demuxer joins a stream mid-section.
    pub unaligned: u64,
    /// Partial sections abandoned at a continuity discontinuity.
    pub abandoned: u64,
    /// Complete sections handed to the caller.
    pub emitted: u64,
}

/// Reassembles sections arriving on one PID.
///
/// One instance per PID. Feed it every payload from that PID in order,
/// together with the packet's `payload_unit_start_indicator`, and it calls
/// back with each complete section exactly once.
#[derive(Debug)]
pub struct SectionAssembler {
    buf: [u8; MAX_SECTION_LEN],
    /// Bytes of the section in progress.
    len: usize,
    /// Total expected, or zero while the three header bytes are still
    /// arriving.
    want: usize,
    /// Whether a `pointer_field` has ever told us where a section starts.
    /// Until then every byte is mid-section and must be dropped.
    aligned: bool,
    stats: AssemblerStats,
}

impl Default for SectionAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl SectionAssembler {
    /// An assembler that has not yet seen a section start.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_SECTION_LEN],
            len: 0,
            want: 0,
            aligned: false,
            stats: AssemblerStats {
                bad_pointer: 0,
                over_long: 0,
                unaligned: 0,
                abandoned: 0,
                emitted: 0,
            },
        }
    }

    /// What has been dropped and why.
    #[must_use]
    pub const fn stats(&self) -> AssemblerStats {
        self.stats
    }

    /// Forget the section in progress **and** the alignment.
    ///
    /// Called on a continuity-counter gap and after a seek. Both lose the same
    /// two things. The first is obvious: the missing packet may have contained
    /// any part of the section, so what is held is worthless.
    ///
    /// The second is not, and a property test found it. Keeping alignment
    /// across a gap makes the assembler read the *middle* of the next section
    /// as a header — the counterexample was a section whose body was zeros,
    /// which frames as an endless run of three-byte sections — and emit
    /// tables nobody sent. The CRC would reject them, but only after the
    /// framing had already spliced unrelated bytes together, and a private
    /// section carries no CRC at all. The only correct answer to lost bytes is
    /// to wait for the next `pointer_field`.
    pub const fn abandon(&mut self) {
        if self.len > 0 {
            self.stats.abandoned = self.stats.abandoned.saturating_add(1);
        }
        self.len = 0;
        self.want = 0;
        self.aligned = false;
    }

    /// Feed one transport packet's payload.
    ///
    /// `f` is called once per complete section, with the whole section
    /// including its CRC. It may be called several times for one payload — a
    /// PAT and the first bytes of an SDT can share a packet — and not at all.
    pub fn push(&mut self, payload_unit_start: bool, payload: &[u8], mut f: impl FnMut(&[u8])) {
        if payload_unit_start {
            let Some((&pointer, rest)) = payload.split_first() else {
                return;
            };
            let skip = usize::from(pointer);
            let Some(tail) = rest.get(skip..) else {
                // A pointer past the end of its own payload is corruption we
                // cannot localise: the section in progress and the one
                // starting are both unreadable.
                self.stats.bad_pointer = self.stats.bad_pointer.saturating_add(1);
                self.abandon();
                self.aligned = false;
                return;
            };
            if let Some(head) = rest.get(..skip) {
                if self.aligned {
                    self.feed(head, &mut f);
                } else {
                    self.stats.unaligned = self.stats.unaligned.saturating_add(head.len() as u64);
                }
            }
            // Whatever state the previous section was in, a new one starts
            // here: any leftover is a section the stream truncated. Alignment
            // is not lost — the pointer field just re-established it.
            if self.len > 0 {
                self.stats.abandoned = self.stats.abandoned.saturating_add(1);
            }
            self.drop_partial();
            self.aligned = true;
            self.feed(tail, &mut f);
        } else if self.aligned {
            self.feed(payload, &mut f);
        } else {
            self.stats.unaligned = self.stats.unaligned.saturating_add(payload.len() as u64);
        }
    }

    /// Forget the section in progress, keeping alignment. Private, because
    /// the only correct *public* answer to lost bytes is to lose alignment
    /// with them.
    const fn drop_partial(&mut self) {
        self.len = 0;
        self.want = 0;
    }

    /// Consume `data`, emitting every section it completes.
    fn feed(&mut self, data: &[u8], f: &mut impl FnMut(&[u8])) {
        let mut rest = data;
        loop {
            if rest.is_empty() {
                return;
            }
            // Between sections, a `0xFF` is stuffing to the end of the packet.
            if self.len == 0 && rest.first() == Some(&STUFFING_BYTE) {
                return;
            }
            if self.want == 0 {
                let taken = self.take(rest, HEADER_MIN);
                rest = rest.get(taken..).unwrap_or(&[]);
                if self.len < HEADER_MIN {
                    return;
                }
                // Only `section_length` is needed here, and it lives entirely
                // in the three bytes just taken. `SectionHeader::parse` cannot
                // be used: it decodes the long-form fields too and refuses a
                // three-byte slice, which is exactly what we have.
                let (Some(&b1), Some(&b2)) = (self.buf.get(1), self.buf.get(2)) else {
                    self.drop_partial();
                    return;
                };
                let section_length = (usize::from(b1 & 0x0F) << 8) | usize::from(b2);
                self.want = HEADER_MIN.saturating_add(section_length);
                if self.want > MAX_SECTION_LEN {
                    // Unreachable: twelve bits cap `total_len` at 4096.
                    self.stats.over_long = self.stats.over_long.saturating_add(1);
                    self.abandon();
                    self.aligned = false;
                    return;
                }
                if self.want <= HEADER_MIN {
                    // A three-byte section: complete already.
                    self.emit(f);
                    continue;
                }
            }
            let want = self.want;
            let taken = self.take(rest, want);
            rest = rest.get(taken..).unwrap_or(&[]);
            if self.len < want {
                return;
            }
            self.emit(f);
        }
    }

    /// Copy from `src` until the buffer holds `target` bytes. Returns how many
    /// were taken.
    fn take(&mut self, src: &[u8], target: usize) -> usize {
        let need = target.saturating_sub(self.len);
        let n = need.min(src.len());
        let (Some(dst), Some(chunk)) = (
            self.buf.get_mut(self.len..self.len.saturating_add(n)),
            src.get(..n),
        ) else {
            return 0;
        };
        dst.copy_from_slice(chunk);
        self.len = self.len.saturating_add(n);
        n
    }

    fn emit(&mut self, f: &mut impl FnMut(&[u8])) {
        if let Some(section) = self.buf.get(..self.len) {
            self.stats.emitted = self.stats.emitted.saturating_add(1);
            f(section);
        }
        self.len = 0;
        self.want = 0;
    }
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
    use crate::crc::crc32;

    /// Build a long-form section with a valid CRC.
    fn section(table_id: u8, ext: u16, version: u8, body: &[u8]) -> Vec<u8> {
        let section_length = 5 + body.len() + 4;
        let mut s = vec![
            table_id,
            0xB0 | ((section_length >> 8) as u8 & 0x0F),
            (section_length & 0xFF) as u8,
            (ext >> 8) as u8,
            (ext & 0xFF) as u8,
            0xC1 | (version << 1),
            0,
            0,
        ];
        s.extend_from_slice(body);
        let crc = crc32(&s);
        s.extend_from_slice(&crc.to_be_bytes());
        s
    }

    /// Chop `data` into payloads of `chunk` bytes, first one preceded by a
    /// zero pointer field.
    fn deliver(a: &mut SectionAssembler, data: &[u8], chunk: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut first = true;
        let mut at = 0;
        while at < data.len() {
            let end = (at + chunk).min(data.len());
            let mut payload = Vec::new();
            if first {
                payload.push(0u8);
            }
            payload.extend_from_slice(&data[at..end]);
            a.push(first, &payload, |s| out.push(s.to_vec()));
            first = false;
            at = end;
        }
        out
    }

    #[test]
    fn a_section_in_one_packet() {
        let s = section(0x00, 1, 3, &[1, 2, 3, 4]);
        let mut a = SectionAssembler::new();
        let got = deliver(&mut a, &s, 4096);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], s);
        let parsed = Section::new(&got[0]).unwrap();
        assert!(parsed.crc_ok());
        assert_eq!(parsed.header.version, 3);
        assert_eq!(parsed.header.table_id_extension, 1);
        assert_eq!(parsed.body().unwrap(), &[1, 2, 3, 4]);
    }

    #[test]
    fn a_section_spanning_packets_at_every_chunk_size() {
        let s = section(0x02, 7, 1, &vec![0xAB; 600]);
        for chunk in 1..=200 {
            let mut a = SectionAssembler::new();
            let got = deliver(&mut a, &s, chunk);
            assert_eq!(got.len(), 1, "chunk {chunk}");
            assert_eq!(got[0], s, "chunk {chunk}");
        }
    }

    #[test]
    fn two_sections_in_one_payload() {
        let a1 = section(0x00, 1, 0, &[1]);
        let a2 = section(0x42, 2, 0, &[2]);
        let mut payload = vec![0u8];
        payload.extend_from_slice(&a1);
        payload.extend_from_slice(&a2);
        payload.push(0xFF);
        let mut a = SectionAssembler::new();
        let mut got = Vec::new();
        a.push(true, &payload, |s| got.push(s.to_vec()));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], a1);
        assert_eq!(got[1], a2);
    }

    #[test]
    fn the_pointer_field_finishes_the_previous_section() {
        let first = section(0x00, 1, 0, &[0x11; 40]);
        let second = section(0x02, 2, 0, &[0x22]);
        let mut a = SectionAssembler::new();
        let mut got = Vec::new();
        // Packet one: pointer 0, then the first 30 bytes of `first`.
        let mut p1 = vec![0u8];
        p1.extend_from_slice(&first[..30]);
        a.push(true, &p1, |s| got.push(s.to_vec()));
        assert!(got.is_empty());
        // Packet two: pointer says the rest of `first` comes first.
        let tail = &first[30..];
        let mut p2 = vec![tail.len() as u8];
        p2.extend_from_slice(tail);
        p2.extend_from_slice(&second);
        a.push(true, &p2, |s| got.push(s.to_vec()));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], first);
        assert_eq!(got[1], second);
    }

    #[test]
    fn bytes_before_the_first_pointer_field_are_dropped() {
        let s = section(0x00, 1, 0, &[9, 9]);
        let mut a = SectionAssembler::new();
        let mut got = Vec::new();
        // A continuation packet arriving before any start.
        a.push(false, &[1, 2, 3, 4], |x| got.push(x.to_vec()));
        assert!(got.is_empty());
        assert_eq!(a.stats().unaligned, 4);
        let mut p = vec![0u8];
        p.extend_from_slice(&s);
        a.push(true, &p, |x| got.push(x.to_vec()));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn a_pointer_past_the_payload_drops_alignment() {
        let mut a = SectionAssembler::new();
        let mut got = Vec::new();
        a.push(true, &[200, 1, 2, 3], |x| got.push(x.to_vec()));
        assert!(got.is_empty());
        assert_eq!(a.stats().bad_pointer, 1);
        // A following continuation packet must not be trusted.
        a.push(false, &[1, 2, 3], |x| got.push(x.to_vec()));
        assert!(got.is_empty());
    }

    #[test]
    fn abandoning_discards_the_partial_section() {
        let s = section(0x00, 1, 0, &vec![0x33; 300]);
        let mut a = SectionAssembler::new();
        let mut got = Vec::new();
        let mut p = vec![0u8];
        p.extend_from_slice(&s[..100]);
        a.push(true, &p, |x| got.push(x.to_vec()));
        a.abandon();
        a.push(false, &s[100..], |x| got.push(x.to_vec()));
        assert!(got.is_empty());
        assert_eq!(a.stats().abandoned, 1);
    }

    #[test]
    fn a_stuffed_payload_emits_nothing() {
        let mut a = SectionAssembler::new();
        let mut got = Vec::new();
        a.push(true, &[0, 0xFF, 0xFF, 0xFF], |x| got.push(x.to_vec()));
        assert!(got.is_empty());
    }

    #[test]
    fn a_corrupt_crc_still_frames_but_fails_the_check() {
        let mut s = section(0x00, 1, 0, &[1, 2, 3]);
        let last = s.len() - 1;
        s[last] ^= 0xFF;
        let mut a = SectionAssembler::new();
        let got = deliver(&mut a, &s, 4096);
        assert_eq!(got.len(), 1);
        let parsed = Section::new(&got[0]).unwrap();
        assert!(!parsed.crc_ok());
        assert!(!parsed.is_applicable());
    }

    #[test]
    fn a_short_form_section_needs_no_crc() {
        // TDT: table_id 0x70, syntax indicator clear.
        let raw = [0x70u8, 0x70, 0x05, 1, 2, 3, 4, 5];
        let s = Section::new(&raw).unwrap();
        assert!(!s.header.syntax);
        assert_eq!(s.header.total_len(), 8);
        assert!(s.crc_ok());
        assert!(!s.is_applicable());
        assert!(s.body().is_none());
    }

    #[test]
    fn the_longest_representable_section_fits() {
        let body = vec![0x5Au8; 4093 - 9];
        let s = section(0x42, 1, 0, &body);
        assert_eq!(s.len(), MAX_SECTION_LEN);
        let mut a = SectionAssembler::new();
        let got = deliver(&mut a, &s, 184);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].len(), MAX_SECTION_LEN);
    }

    #[test]
    fn current_next_zero_is_not_applicable() {
        let mut s = section(0x02, 1, 0, &[1]);
        s[5] &= !0x01;
        let sec = Section::new(&s).unwrap();
        // The CRC no longer matches after the edit; rebuild it.
        let crc = crc32(&s[..s.len() - 4]);
        let mut s2 = s[..s.len() - 4].to_vec();
        s2.extend_from_slice(&crc.to_be_bytes());
        let sec2 = Section::new(&s2).unwrap();
        assert!(!sec.header.current_next);
        assert!(sec2.crc_ok());
        assert!(!sec2.is_applicable());
    }
}
