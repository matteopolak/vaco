//! Section, PAT, PMT and SDT **writers** (ISO/IEC 13818-1 §2.4.4, ETSI EN
//! 300 468 §5.2.3).
//!
//! The mirror image of [`crate::psi`]: that module borrows a section a
//! demuxer already received; this one builds one a muxer is about to send.
//! Both sides are checked against each other directly —
//! `vaco-mux-mpegts`'s tests feed a table built here straight back through
//! [`crate::psi::Pat`]/[`crate::psi::Pmt`]/[`crate::psi::Sdt`] — so a field
//! that drifts between the two shows up as a round-trip failure rather than
//! two crates quietly disagreeing about the wire format.
//!
//! # What this deliberately does not do
//!
//! Nothing here spans a table across more than one section.
//! [`section::MAX_PSI_SECTION_LEN`](crate::section::MAX_PSI_SECTION_LEN)
//! bounds how large a single PAT/PMT/SDT section may be, and multi-section
//! tables only arise from a channel count or a program count this crate's
//! callers — small, single- or few-program streams — do not produce in
//! practice. [`build_section`] reports `None` rather than silently
//! truncating when a body would not fit.

use crate::crc::crc32;
use crate::descriptor::TAG_SERVICE;
use crate::psi::{TABLE_PAT, TABLE_PMT, TABLE_SDT_ACTUAL};
use crate::section::MAX_PSI_SECTION_LEN;

/// Bytes of long-form section header before the body: `table_id`,
/// `section_length` (2 bytes), `table_id_extension` (2 bytes),
/// `version`/`current_next`, `section_number`, `last_section_number`.
const LONG_HEADER_LEN: usize = 8;
/// Trailing CRC-32.
const CRC_LEN: usize = 4;

/// Assemble one long-form PSI section: header, `body`, and its own CRC-32.
///
/// `table_id_extension` is the PAT's `transport_stream_id`, the PMT's
/// `program_number`, or the SDT's `transport_stream_id`.
/// `current_next_indicator` is always set — this crate never emits a table
/// describing a *future* configuration.
///
/// `reserved_future_use` is the bit immediately after
/// `section_syntax_indicator`, and the two specifications disagree about it:
/// it is `private_indicator` in ISO/IEC 13818-1's tables (PAT, PMT), which set
/// it to 0, and `reserved_future_use` in ETSI EN 300 468's (SDT), which sets it
/// to 1. Emitting the ISO value for an SDT is a one-bit divergence that shows
/// up as `0xB0` where the reference writes `0xF0`, and it was ours until it was
/// measured.
///
/// Returns `None` when the assembled section would not fit the 12-bit
/// `section_length` field's own limit for `section_syntax_indicator == 1`
/// tables (§2.4.4.10), which is smaller than the field's raw ceiling.
#[must_use]
pub fn build_section(
    table_id: u8,
    table_id_extension: u16,
    version: u8,
    section_number: u8,
    last_section_number: u8,
    reserved_future_use: bool,
    body: &[u8],
) -> Option<Vec<u8>> {
    // Bytes after `section_length` itself: ext + version byte + two section
    // numbers + body + CRC.
    let section_length = (LONG_HEADER_LEN - 3)
        .checked_add(body.len())?
        .checked_add(CRC_LEN)?;
    let total = 3usize.checked_add(section_length)?;
    if total > MAX_PSI_SECTION_LEN || section_length > 0x0FFF {
        return None;
    }
    let mut s = Vec::new();
    s.push(table_id);
    // section_syntax_indicator=1, then the disputed bit, then reserved='11'.
    let flags = if reserved_future_use { 0xF0 } else { 0xB0 };
    s.push(flags | ((section_length >> 8) as u8 & 0x0F));
    s.push((section_length & 0xFF) as u8);
    s.extend_from_slice(&table_id_extension.to_be_bytes());
    // reserved='11', version_number (5 bits), current_next_indicator=1.
    s.push(0xC1 | ((version & 0x1F) << 1));
    s.push(section_number);
    s.push(last_section_number);
    s.extend_from_slice(body);
    let crc = crc32(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    Some(s)
}

/// Encode one descriptor: `tag`, length byte, `payload`. `None` when
/// `payload` is longer than a descriptor's one-byte length can state.
#[must_use]
pub fn build_descriptor(tag: u8, payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() > 0xFF {
        return None;
    }
    let mut d = Vec::new();
    d.push(tag);
    d.push(payload.len() as u8);
    d.extend_from_slice(payload);
    Some(d)
}

/// `registration_descriptor` (§2.6.8): a four-byte format identifier.
#[must_use]
pub fn registration_descriptor(id: [u8; 4]) -> Vec<u8> {
    // Four bytes always fits a one-byte length; `build_descriptor` cannot
    // return `None` here.
    build_descriptor(crate::descriptor::TAG_REGISTRATION, &id).unwrap_or_default()
}

/// One PAT row to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatEntryOut {
    pub program_number: u16,
    pub pid: u16,
}

/// Build a single-section PAT.
///
/// `None` when the program list does not fit one section — every input this
/// crate's muxer produces has one or a handful of programs, so this is a
/// generous ceiling in practice (roughly 250 programs).
#[must_use]
pub fn write_pat(
    transport_stream_id: u16,
    version: u8,
    entries: &[PatEntryOut],
) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    for e in entries {
        body.extend_from_slice(&e.program_number.to_be_bytes());
        // reserved='111', pid (13 bits).
        body.push(0xE0 | ((e.pid >> 8) as u8 & 0x1F));
        body.push((e.pid & 0xFF) as u8);
    }
    build_section(TABLE_PAT, transport_stream_id, version, 0, 0, false, &body)
}

/// One elementary stream declaration to write into a PMT.
#[derive(Debug, Clone)]
pub struct PmtStreamOut {
    pub stream_type: u8,
    pub elementary_pid: u16,
    /// Already-encoded `ES_info` descriptor loop (see [`build_descriptor`]).
    pub descriptors: Vec<u8>,
}

/// Build a single-section PMT.
///
/// `pcr_pid` of `0x1FFF` means the program has no PCR stream, which is legal
/// (§2.4.4.8) though a muxer normally does carry one.
///
/// `None` when the stream list and descriptors do not fit one section.
#[must_use]
pub fn write_pmt(
    program_number: u16,
    version: u8,
    pcr_pid: u16,
    program_descriptors: &[u8],
    streams: &[PmtStreamOut],
) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    body.push(0xE0 | ((pcr_pid >> 8) as u8 & 0x1F));
    body.push((pcr_pid & 0xFF) as u8);
    let info_len = program_descriptors.len();
    if info_len > 0x0FFF {
        return None;
    }
    body.push(0xF0 | ((info_len >> 8) as u8 & 0x0F));
    body.push((info_len & 0xFF) as u8);
    body.extend_from_slice(program_descriptors);
    for s in streams {
        body.push(s.stream_type);
        body.push(0xE0 | ((s.elementary_pid >> 8) as u8 & 0x1F));
        body.push((s.elementary_pid & 0xFF) as u8);
        let es_len = s.descriptors.len();
        if es_len > 0x0FFF {
            return None;
        }
        body.push(0xF0 | ((es_len >> 8) as u8 & 0x0F));
        body.push((es_len & 0xFF) as u8);
        body.extend_from_slice(&s.descriptors);
    }
    build_section(TABLE_PMT, program_number, version, 0, 0, false, &body)
}

/// One SDT service to write.
#[derive(Debug, Clone)]
pub struct SdtServiceOut {
    pub service_id: u16,
    pub eit_schedule: bool,
    pub eit_present_following: bool,
    pub running_status: u8,
    pub free_ca_mode: bool,
    /// Already-encoded descriptor loop, typically one [`service_descriptor`].
    pub descriptors: Vec<u8>,
}

/// EN 300 468 §6.2.33 `service_descriptor`: service type plus the provider
/// and service names. Names are written as-is (no DVB text control byte),
/// which is correct for the default ISO 6937 / Latin alphabet repertoire and
/// is all `-metadata service_name`/`service_provider` need; a caller wanting
/// another repertoire must prepend its own encoding byte before calling this.
#[must_use]
pub fn service_descriptor(service_type: u8, provider: &[u8], name: &[u8]) -> Option<Vec<u8>> {
    if provider.len() > 0xFF || name.len() > 0xFF {
        return None;
    }
    let mut payload = Vec::new();
    payload.push(service_type);
    payload.push(provider.len() as u8);
    payload.extend_from_slice(provider);
    payload.push(name.len() as u8);
    payload.extend_from_slice(name);
    build_descriptor(TAG_SERVICE, &payload)
}

/// Build a single-section SDT (`table_id` `0x42`, "actual" transport stream).
///
/// `None` when the service list does not fit one section.
#[must_use]
pub fn write_sdt(
    transport_stream_id: u16,
    original_network_id: u16,
    version: u8,
    services: &[SdtServiceOut],
) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    body.extend_from_slice(&original_network_id.to_be_bytes());
    body.push(0xFF); // reserved_future_use, all ones.
    for s in services {
        body.extend_from_slice(&s.service_id.to_be_bytes());
        // reserved_future_use='111111', EIT_schedule_flag, EIT_present_following_flag.
        body.push(0xFC | (u8::from(s.eit_schedule) << 1) | u8::from(s.eit_present_following));
        let len = s.descriptors.len();
        if len > 0x0FFF {
            return None;
        }
        body.push(
            ((s.running_status & 0x07) << 5)
                | (u8::from(s.free_ca_mode) << 4)
                | ((len >> 8) as u8 & 0x0F),
        );
        body.push((len & 0xFF) as u8);
        body.extend_from_slice(&s.descriptors);
    }
    build_section(TABLE_SDT_ACTUAL, transport_stream_id, version, 0, 0, true, &body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    /// The bit after `section_syntax_indicator` differs between the two
    /// specifications, and using the ISO value everywhere is a one-bit
    /// divergence that survives every structural check.
    ///
    /// Measured on `ffmpeg -c copy -f mpegts`: its SDT section header is
    /// `42 f0 25` and its PAT's is `00 b0 0d`. Ours wrote `42 b0 25`.
    #[test]
    fn the_sdt_sets_reserved_future_use_and_the_pat_does_not() {
        let sdt = super::write_sdt(
            1,
            0xff01,
            0,
            &[super::SdtServiceOut {
                service_id: 1,
                eit_schedule: false,
                eit_present_following: false,
                running_status: 4,
                free_ca_mode: false,
                descriptors: Vec::new(),
            }],
        )
        .unwrap();
        assert_eq!(sdt[0], 0x42);
        assert_eq!(sdt[1] & 0xF0, 0xF0, "SDT: {:02x?}", &sdt[..3]);

        let pat = super::write_pat(1, 0, &[super::PatEntryOut {
            program_number: 1,
            pid: 0x1000,
        }])
        .unwrap();
        assert_eq!(pat[0], 0x00);
        assert_eq!(pat[1] & 0xF0, 0xB0, "PAT: {:02x?}", &pat[..3]);
    }

    use super::*;
    use crate::psi::{Pat, Pmt, Sdt};
    use crate::section::Section;

    #[test]
    fn a_written_pat_reads_back_through_the_parser() {
        let entries = [
            PatEntryOut {
                program_number: 0,
                pid: 0x0010,
            },
            PatEntryOut {
                program_number: 1,
                pid: 0x1000,
            },
        ];
        let raw = write_pat(7, 3, &entries).unwrap();
        let section = Section::new(&raw).unwrap();
        assert!(section.is_applicable());
        let pat = Pat::parse(&section).unwrap();
        assert_eq!(pat.transport_stream_id, 7);
        assert_eq!(pat.version, 3);
        let got: Vec<_> = pat.entries().collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].pid, 0x0010);
        assert_eq!(got[1].pid, 0x1000);
        assert_eq!(pat.nit_pid(), Some(0x0010));
    }

    #[test]
    fn a_written_pmt_reads_back_through_the_parser() {
        let streams = vec![
            PmtStreamOut {
                stream_type: 0x1B,
                elementary_pid: 0x100,
                descriptors: Vec::new(),
            },
            PmtStreamOut {
                stream_type: 0x0F,
                elementary_pid: 0x101,
                descriptors: build_descriptor(crate::descriptor::TAG_ISO639_LANGUAGE, b"eng\x00")
                    .unwrap(),
            },
        ];
        let raw = write_pmt(1, 5, 0x100, &[], &streams).unwrap();
        let section = Section::new(&raw).unwrap();
        assert!(section.is_applicable());
        let pmt = Pmt::parse(&section).unwrap();
        assert_eq!(pmt.program_number, 1);
        assert_eq!(pmt.version, 5);
        assert_eq!(pmt.pcr_pid, 0x100);
        let got: Vec<_> = pmt.streams().collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].stream_type, 0x1B);
        assert_eq!(got[1].elementary_pid, 0x101);
        let lang = got[1]
            .find_descriptor(crate::descriptor::TAG_ISO639_LANGUAGE)
            .unwrap();
        assert_eq!(
            lang.iso639_languages().next().unwrap().as_str(),
            Some("eng")
        );
    }

    #[test]
    fn a_written_sdt_reads_back_through_the_parser() {
        let desc = service_descriptor(0x01, b"vaco", b"Service01").unwrap();
        let services = vec![SdtServiceOut {
            service_id: 1,
            eit_schedule: false,
            eit_present_following: true,
            running_status: 4,
            free_ca_mode: false,
            descriptors: desc,
        }];
        let raw = write_sdt(1, 0x00FF, 0, &services).unwrap();
        let section = Section::new(&raw).unwrap();
        assert!(section.is_applicable());
        let sdt = Sdt::parse(&section).unwrap();
        assert!(sdt.actual);
        assert_eq!(sdt.original_network_id, 0x00FF);
        let got: Vec<_> = sdt.services().collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].service_id, 1);
        assert!(got[0].eit_present_following);
        assert!(!got[0].eit_schedule);
        assert_eq!(got[0].running_status, 4);
        assert_eq!(
            got[0].names(),
            Some(("vaco".to_owned(), "Service01".to_owned()))
        );
    }

    #[test]
    fn registration_descriptor_round_trips_through_the_generic_iterator() {
        let raw = registration_descriptor(*b"AC-3");
        let iter = crate::descriptor::DescriptorIter::new(&raw);
        assert_eq!(iter.registration(), Some(*b"AC-3"));
    }

    #[test]
    fn a_body_too_large_for_one_section_is_refused_not_truncated() {
        // Enough streams to push a PMT section past MAX_PSI_SECTION_LEN.
        let streams: Vec<PmtStreamOut> = (0..300u16)
            .map(|i| PmtStreamOut {
                stream_type: 0x1B,
                elementary_pid: 0x100 + i,
                descriptors: Vec::new(),
            })
            .collect();
        assert!(write_pmt(1, 0, 0x100, &[], &streams).is_none());
    }
}
