//! Writing one Partition Pack — the same fixed-position layout
//! `vaco-demux-mxf::partition::parse` reads (the pack predates the local-set
//! convention and this crate does not adopt it here either).

use vaco_core::Result;
use vaco_io::IoWriter;

use crate::ul::Ul;

/// Everything one partition pack states about itself and the file around it.
#[derive(Debug, Clone)]
pub(crate) struct PartitionPackFields {
    pub this_partition: u64,
    pub previous_partition: u64,
    pub footer_partition: u64,
    pub header_byte_count: u64,
    pub index_byte_count: u64,
    pub index_sid: u32,
    pub body_offset: u64,
    pub body_sid: u32,
    pub operational_pattern: Ul,
    pub essence_containers: Vec<Ul>,
}

/// Write `key` followed by the partition pack's fixed-layout value.
///
/// KAG size is fixed at `1` (no alignment grid — this crate does not pad
/// with Fill Items, matching `vaco-demux-mxf`'s own "reads forward by key,
/// never trusts byte-count arithmetic" stance: nothing downstream needs
/// KAG alignment to parse this file correctly).
///
/// # Errors
/// Propagates I/O failure.
pub(crate) fn write(io: &mut IoWriter, key: &[u8; 16], fields: &PartitionPackFields) -> Result<()> {
    let mut value = Vec::new();
    value.extend_from_slice(&1u16.to_be_bytes()); // major version
    // Minor version 3, not 2: measured this session against every real
    // ffmpeg -f mxf/-f mxf_d10 fixture in this workspace's corpus
    // (op1a_mpeg2_sample.mxf, a real two-track file) — the first byte a
    // literal `cmp` against a real `-fflags +bitexact` file actually
    // disagreed on, ahead of anything ID-related.
    value.extend_from_slice(&3u16.to_be_bytes()); // minor version

    value.extend_from_slice(&1u32.to_be_bytes()); // KAGSize
    value.extend_from_slice(&fields.this_partition.to_be_bytes());
    value.extend_from_slice(&fields.previous_partition.to_be_bytes());
    value.extend_from_slice(&fields.footer_partition.to_be_bytes());
    value.extend_from_slice(&fields.header_byte_count.to_be_bytes());
    value.extend_from_slice(&fields.index_byte_count.to_be_bytes());
    value.extend_from_slice(&fields.index_sid.to_be_bytes());
    value.extend_from_slice(&fields.body_offset.to_be_bytes());
    value.extend_from_slice(&fields.body_sid.to_be_bytes());
    value.extend_from_slice(&fields.operational_pattern.as_bytes());
    value.extend_from_slice(&(fields.essence_containers.len() as u32).to_be_bytes());
    value.extend_from_slice(&16u32.to_be_bytes());
    for ec in &fields.essence_containers {
        value.extend_from_slice(&ec.as_bytes());
    }
    crate::klv::write(io, key, &value)
}

/// Byte offset, within a partition pack's *value*, of the `FooterPartition`
/// field — for the one small backpatch this crate performs on a seekable
/// sink (see `mux.rs`'s module docs): major(2) + minor(2) + KAGSize(4) +
/// ThisPartition(8) + PreviousPartition(8) = 24.
pub(crate) const FOOTER_PARTITION_FIELD_OFFSET: u64 = 24;
