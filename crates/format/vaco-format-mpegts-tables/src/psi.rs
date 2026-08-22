//! The tables a demuxer acts on: PAT, PMT, CAT and SDT.
//!
//! Each is a borrowed view over a section that [`crate::section::Section`] has
//! already framed and CRC-checked. Nothing here allocates and nothing here
//! decides policy: a PMT with a `version_number` the caller has already seen is
//! still parsed, because whether to apply it is the demuxer's
//! `merge_pmt_versions` question, not this layer's.

use crate::descriptor::{Descriptor, DescriptorIter};
use crate::section::Section;

/// `table_id` of the Program Association Table.
pub const TABLE_PAT: u8 = 0x00;
/// `table_id` of the Conditional Access Table.
pub const TABLE_CAT: u8 = 0x01;
/// `table_id` of a Program Map Table.
pub const TABLE_PMT: u8 = 0x02;
/// `table_id` of the Transport Stream Description Table.
pub const TABLE_TSDT: u8 = 0x03;
/// DVB Network Information Table, this network.
pub const TABLE_NIT_ACTUAL: u8 = 0x40;
/// DVB Network Information Table, another network.
pub const TABLE_NIT_OTHER: u8 = 0x41;
/// DVB Service Description Table, this transport stream.
pub const TABLE_SDT_ACTUAL: u8 = 0x42;
/// DVB Service Description Table, another transport stream.
pub const TABLE_SDT_OTHER: u8 = 0x46;
/// DVB Time and Date Table.
pub const TABLE_TDT: u8 = 0x70;
/// DVB Time Offset Table.
pub const TABLE_TOT: u8 = 0x73;

/// A `program_number` of zero names the NIT PID rather than a PMT PID
/// (13818-1 §2.4.4.5).
pub const PROGRAM_NUMBER_NIT: u16 = 0;

// ---------------------------------------------------------------------- PAT

/// The Program Association Table.
#[derive(Debug, Clone, Copy)]
pub struct Pat<'a> {
    /// `table_id_extension` of a PAT is the `transport_stream_id`.
    pub transport_stream_id: u16,
    pub version: u8,
    body: &'a [u8],
}

impl<'a> Pat<'a> {
    /// Interpret `section` as a PAT, or `None` if it is not one.
    ///
    /// Requires the section to be applicable — long form, CRC valid,
    /// `current_next_indicator` set — because a demuxer that acts on a PAT it
    /// has not checked will happily build programs out of noise.
    #[must_use]
    pub fn parse(section: &Section<'a>) -> Option<Self> {
        if section.header.table_id != TABLE_PAT || !section.is_applicable() {
            return None;
        }
        Some(Self {
            transport_stream_id: section.header.table_id_extension,
            version: section.header.version,
            body: section.body()?,
        })
    }

    /// The `(program_number, pid)` pairs, in transmission order.
    #[must_use]
    pub fn entries(&self) -> PatIter<'a> {
        PatIter { rest: self.body }
    }

    /// The PID the Network Information Table is on, if the PAT names one.
    #[must_use]
    pub fn nit_pid(&self) -> Option<u16> {
        self.entries()
            .find(|e| e.program_number == PROGRAM_NUMBER_NIT)
            .map(|e| e.pid)
    }
}

/// One PAT row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatEntry {
    pub program_number: u16,
    /// The PMT's PID, or the NIT's when `program_number` is zero.
    pub pid: u16,
}

/// Iterator over PAT rows.
#[derive(Debug, Clone)]
pub struct PatIter<'a> {
    rest: &'a [u8],
}

impl Iterator for PatIter<'_> {
    type Item = PatEntry;

    fn next(&mut self) -> Option<Self::Item> {
        let e = self.rest.get(..4)?;
        let entry = PatEntry {
            program_number: u16::from_be_bytes([*e.first()?, *e.get(1)?]),
            pid: (u16::from(*e.get(2)? & 0x1F) << 8) | u16::from(*e.get(3)?),
        };
        self.rest = self.rest.get(4..).unwrap_or(&[]);
        Some(entry)
    }
}

// ---------------------------------------------------------------------- PMT

/// A Program Map Table.
#[derive(Debug, Clone, Copy)]
pub struct Pmt<'a> {
    /// `table_id_extension` of a PMT is the `program_number`.
    pub program_number: u16,
    pub version: u8,
    /// PID carrying this program's Program Clock Reference. `0x1FFF` means the
    /// program has none, which is legal and means duration and seeking have to
    /// come from PTS alone.
    pub pcr_pid: u16,
    program_info: &'a [u8],
    streams: &'a [u8],
}

impl<'a> Pmt<'a> {
    /// Interpret `section` as a PMT.
    #[must_use]
    pub fn parse(section: &Section<'a>) -> Option<Self> {
        if section.header.table_id != TABLE_PMT || !section.is_applicable() {
            return None;
        }
        let body = section.body()?;
        let pcr_pid = (u16::from(*body.first()? & 0x1F) << 8) | u16::from(*body.get(1)?);
        let info_len = (usize::from(*body.get(2)? & 0x0F) << 8) | usize::from(*body.get(3)?);
        let info_end = 4usize.checked_add(info_len)?;
        Some(Self {
            program_number: section.header.table_id_extension,
            version: section.header.version,
            pcr_pid,
            program_info: body.get(4..info_end)?,
            streams: body.get(info_end..)?,
        })
    }

    /// The program-level descriptor loop.
    #[must_use]
    pub const fn program_descriptors(&self) -> DescriptorIter<'a> {
        DescriptorIter::new(self.program_info)
    }

    /// The elementary streams this program declares.
    #[must_use]
    pub const fn streams(&self) -> PmtStreamIter<'a> {
        PmtStreamIter { rest: self.streams }
    }
}

/// One elementary stream declaration inside a PMT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmtStream<'a> {
    pub stream_type: u8,
    pub elementary_pid: u16,
    /// The `ES_info` descriptor loop, unparsed.
    pub descriptors: &'a [u8],
}

impl<'a> PmtStream<'a> {
    /// Walk this stream's descriptors.
    #[must_use]
    pub const fn descriptor_iter(&self) -> DescriptorIter<'a> {
        DescriptorIter::new(self.descriptors)
    }

    /// The first descriptor with `tag`.
    #[must_use]
    pub fn find_descriptor(&self, tag: u8) -> Option<Descriptor<'a>> {
        self.descriptor_iter().find_tag(tag)
    }
}

/// Iterator over a PMT's elementary streams.
#[derive(Debug, Clone)]
pub struct PmtStreamIter<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for PmtStreamIter<'a> {
    type Item = PmtStream<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let head = self.rest.get(..5)?;
        let stream_type = *head.first()?;
        let elementary_pid = (u16::from(*head.get(1)? & 0x1F) << 8) | u16::from(*head.get(2)?);
        let len = (usize::from(*head.get(3)? & 0x0F) << 8) | usize::from(*head.get(4)?);
        let end = 5usize.checked_add(len)?;
        let descriptors = self.rest.get(5..end)?;
        self.rest = self.rest.get(end..).unwrap_or(&[]);
        Some(PmtStream {
            stream_type,
            elementary_pid,
            descriptors,
        })
    }
}

// ---------------------------------------------------------------------- CAT

/// The Conditional Access Table: a bare descriptor loop.
#[derive(Debug, Clone, Copy)]
pub struct Cat<'a> {
    pub version: u8,
    descriptors: &'a [u8],
}

impl<'a> Cat<'a> {
    /// Interpret `section` as a CAT.
    #[must_use]
    pub fn parse(section: &Section<'a>) -> Option<Self> {
        if section.header.table_id != TABLE_CAT || !section.is_applicable() {
            return None;
        }
        Some(Self {
            version: section.header.version,
            descriptors: section.body()?,
        })
    }

    /// The CA descriptors.
    #[must_use]
    pub const fn descriptors(&self) -> DescriptorIter<'a> {
        DescriptorIter::new(self.descriptors)
    }
}

// ---------------------------------------------------------------------- SDT

/// A DVB Service Description Table (EN 300 468 §5.2.3).
#[derive(Debug, Clone, Copy)]
pub struct Sdt<'a> {
    /// `table_id_extension` of an SDT is the `transport_stream_id`.
    pub transport_stream_id: u16,
    pub original_network_id: u16,
    pub version: u8,
    /// Whether this SDT describes the transport stream it arrived on.
    pub actual: bool,
    body: &'a [u8],
}

impl<'a> Sdt<'a> {
    /// Interpret `section` as an SDT.
    #[must_use]
    pub fn parse(section: &Section<'a>) -> Option<Self> {
        let actual = match section.header.table_id {
            TABLE_SDT_ACTUAL => true,
            TABLE_SDT_OTHER => false,
            _ => return None,
        };
        if !section.is_applicable() {
            return None;
        }
        let body = section.body()?;
        Some(Self {
            transport_stream_id: section.header.table_id_extension,
            original_network_id: u16::from_be_bytes([*body.first()?, *body.get(1)?]),
            version: section.header.version,
            actual,
            // One reserved byte follows `original_network_id`.
            body: body.get(3..)?,
        })
    }

    /// The services this table describes.
    #[must_use]
    pub const fn services(&self) -> SdtServiceIter<'a> {
        SdtServiceIter { rest: self.body }
    }
}

/// One SDT service entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdtService<'a> {
    /// Equal to the `program_number` in the PAT.
    pub service_id: u16,
    pub eit_schedule: bool,
    pub eit_present_following: bool,
    /// `0` undefined, `1` not running, `2` starts shortly, `3` pausing,
    /// `4` running, `5` off-air.
    pub running_status: u8,
    /// Whether any component of the service is CA-scrambled.
    pub free_ca_mode: bool,
    pub descriptors: &'a [u8],
}

impl<'a> SdtService<'a> {
    /// Walk this service's descriptors.
    #[must_use]
    pub const fn descriptor_iter(&self) -> DescriptorIter<'a> {
        DescriptorIter::new(self.descriptors)
    }

    /// `(provider_name, service_name)`, decoded from DVB text.
    ///
    /// These are exactly the two tags `vaco-probe -show_programs` prints as
    /// `service_provider` and `service_name`.
    #[must_use]
    pub fn names(&self) -> Option<(String, String)> {
        let d = self
            .descriptor_iter()
            .find_tag(crate::descriptor::TAG_SERVICE)?;
        let s = d.service()?;
        Some((crate::text::decode(s.provider), crate::text::decode(s.name)))
    }

    /// The DVB `service_type`, when a `service_descriptor` is present.
    #[must_use]
    pub fn service_type(&self) -> Option<u8> {
        self.descriptor_iter()
            .find_tag(crate::descriptor::TAG_SERVICE)
            .and_then(|d| d.service())
            .map(|s| s.service_type)
    }
}

/// Iterator over SDT services.
#[derive(Debug, Clone)]
pub struct SdtServiceIter<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for SdtServiceIter<'a> {
    type Item = SdtService<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let head = self.rest.get(..5)?;
        let service_id = u16::from_be_bytes([*head.first()?, *head.get(1)?]);
        let flags = *head.get(2)?;
        let b3 = *head.get(3)?;
        let len = (usize::from(b3 & 0x0F) << 8) | usize::from(*head.get(4)?);
        let end = 5usize.checked_add(len)?;
        let descriptors = self.rest.get(5..end)?;
        self.rest = self.rest.get(end..).unwrap_or(&[]);
        Some(SdtService {
            service_id,
            eit_schedule: flags & 0x02 != 0,
            eit_present_following: flags & 0x01 != 0,
            running_status: b3 >> 5,
            free_ca_mode: b3 & 0x10 != 0,
            descriptors,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::crc::crc32;

    fn build(table_id: u8, ext: u16, version: u8, body: &[u8]) -> Vec<u8> {
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
        s.extend_from_slice(&crc32(&s).to_be_bytes());
        s
    }

    #[test]
    fn a_two_program_pat() {
        let body = [0x00, 0x00, 0xE0, 0x10, 0x00, 0x01, 0xF0, 0x00];
        let raw = build(TABLE_PAT, 1, 0, &body);
        let section = Section::new(&raw).unwrap();
        let pat = Pat::parse(&section).unwrap();
        assert_eq!(pat.transport_stream_id, 1);
        let e: Vec<_> = pat.entries().collect();
        assert_eq!(e.len(), 2);
        assert_eq!(
            e[0],
            PatEntry {
                program_number: 0,
                pid: 0x0010
            }
        );
        assert_eq!(
            e[1],
            PatEntry {
                program_number: 1,
                pid: 0x1000
            }
        );
        assert_eq!(pat.nit_pid(), Some(0x0010));
    }

    #[test]
    fn a_pat_with_a_bad_crc_is_refused() {
        let mut raw = build(TABLE_PAT, 1, 0, &[0, 1, 0xF0, 0x00]);
        let last = raw.len() - 1;
        raw[last] ^= 1;
        let section = Section::new(&raw).unwrap();
        assert!(Pat::parse(&section).is_none());
    }

    #[test]
    fn a_pmt_with_two_streams_and_descriptors() {
        let mut body = vec![0xE1, 0x00, 0xF0, 0x00];
        // Video: stream_type 0x1B, PID 0x100, no descriptors.
        body.extend_from_slice(&[0x1B, 0xE1, 0x00, 0xF0, 0x00]);
        // Audio: stream_type 0x0F, PID 0x101, one language descriptor.
        body.extend_from_slice(&[0x0F, 0xE1, 0x01, 0xF0, 0x06]);
        body.extend_from_slice(&[0x0A, 0x04, b'e', b'n', b'g', 0x00]);
        let raw = build(TABLE_PMT, 1, 3, &body);
        let section = Section::new(&raw).unwrap();
        let pmt = Pmt::parse(&section).unwrap();
        assert_eq!(pmt.program_number, 1);
        assert_eq!(pmt.version, 3);
        assert_eq!(pmt.pcr_pid, 0x100);
        let s: Vec<_> = pmt.streams().collect();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].stream_type, 0x1B);
        assert_eq!(s[1].elementary_pid, 0x101);
        let lang = s[1]
            .find_descriptor(crate::descriptor::TAG_ISO639_LANGUAGE)
            .unwrap();
        assert_eq!(
            lang.iso639_languages().next().unwrap().as_str(),
            Some("eng")
        );
    }

    #[test]
    fn a_pmt_whose_program_info_length_lies_is_refused() {
        let body = vec![0xE1, 0x00, 0xF0, 0xFF];
        let raw = build(TABLE_PMT, 1, 0, &body);
        let section = Section::new(&raw).unwrap();
        assert!(Pmt::parse(&section).is_none());
    }

    #[test]
    fn a_pmt_whose_es_info_length_lies_stops_the_iterator() {
        let mut body = vec![0xE1, 0x00, 0xF0, 0x00];
        body.extend_from_slice(&[0x1B, 0xE1, 0x00, 0xF0, 0xFF]);
        let raw = build(TABLE_PMT, 1, 0, &body);
        let section = Section::new(&raw).unwrap();
        let pmt = Pmt::parse(&section).unwrap();
        assert_eq!(pmt.streams().count(), 0);
    }

    #[test]
    fn a_pcr_pid_of_all_ones_means_no_pcr() {
        let body = vec![0xFF, 0xFF, 0xF0, 0x00];
        let raw = build(TABLE_PMT, 1, 0, &body);
        let section = Section::new(&raw).unwrap();
        assert_eq!(Pmt::parse(&section).unwrap().pcr_pid, 0x1FFF);
    }

    #[test]
    fn an_sdt_carries_the_service_names_the_probe_prints() {
        let mut svc_desc = vec![0x48u8, 0, 0x01, 6];
        svc_desc.extend_from_slice(b"FFmpeg");
        svc_desc.push(9);
        svc_desc.extend_from_slice(b"Service01");
        svc_desc[1] = (svc_desc.len() - 2) as u8;
        let mut body = vec![0x00, 0x01, 0xFF];
        body.extend_from_slice(&[0x00, 0x01, 0xFF, 0x80 | ((svc_desc.len() >> 8) as u8)]);
        body.push((svc_desc.len() & 0xFF) as u8);
        body.extend_from_slice(&svc_desc);
        let raw = build(TABLE_SDT_ACTUAL, 1, 0, &body);
        let section = Section::new(&raw).unwrap();
        let sdt = Sdt::parse(&section).unwrap();
        assert!(sdt.actual);
        assert_eq!(sdt.original_network_id, 1);
        let s: Vec<_> = sdt.services().collect();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].service_id, 1);
        assert_eq!(
            s[0].names(),
            Some(("FFmpeg".to_owned(), "Service01".to_owned()))
        );
        assert_eq!(s[0].service_type(), Some(0x01));
    }

    #[test]
    fn tables_refuse_each_others_sections() {
        let raw = build(TABLE_PAT, 1, 0, &[0, 1, 0xF0, 0x00]);
        let section = Section::new(&raw).unwrap();
        assert!(Pmt::parse(&section).is_none());
        assert!(Cat::parse(&section).is_none());
        assert!(Sdt::parse(&section).is_none());
        assert!(Pat::parse(&section).is_some());
    }

    #[test]
    fn a_cat_is_a_descriptor_loop() {
        let raw = build(TABLE_CAT, 0xFFFF, 0, &[0x09, 0x04, 0x00, 0x01, 0xE0, 0x50]);
        let section = Section::new(&raw).unwrap();
        let cat = Cat::parse(&section).unwrap();
        assert_eq!(cat.descriptors().count(), 1);
    }

    #[test]
    fn a_truncated_table_yields_nothing_rather_than_panicking() {
        for len in 0..12usize {
            let raw = vec![TABLE_PMT; len];
            if let Some(section) = Section::new(&raw) {
                assert!(Pmt::parse(&section).is_none());
                assert!(Pat::parse(&section).is_none());
            }
        }
    }
}
