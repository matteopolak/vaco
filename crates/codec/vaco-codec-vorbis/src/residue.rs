//! Residue setup and decode, types 0, 1 and 2 (spec section 8).
//!
//! `Vaco-Spec-Ref: vorbis-i sections 8.6.1 through 8.6.5`

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::bitreader::BitReaderLsb;
use crate::codebook::Codebook;

#[derive(Debug, Clone)]
pub(crate) struct ResidueConfig {
    pub(crate) residue_type: u8,
    begin: u32,
    end: u32,
    partition_size: u32,
    classifications: u32,
    classbook: u8,
    /// One row per classification, one column per of the 8 possible passes;
    /// `-1` means that classification codes nothing in that pass.
    books: Vec<[i16; 8]>,
}

impl ResidueConfig {
    pub(crate) fn parse_header(
        residue_type: u8,
        r: &mut BitReaderLsb<'_>,
        budget: &mut Budget,
        codebooks: &[Codebook],
    ) -> Result<Self> {
        let begin = r.get(24);
        let end = r.get(24);
        let partition_size = r.get(24).saturating_add(1);
        let classifications = r.get(6).saturating_add(1);
        let classbook = r.get(8);
        // The classbook is read in *scalar* context only (it codes
        // classification numbers, not residue values), so unlike the
        // per-classification books below it is not required to carry a
        // value mapping — spec section 8.6.1 states the requirement for
        // `residue_books` alone.
        let Some(cb) = codebooks.get(classbook as usize) else {
            return Err(Error::InvalidData("vorbis: residue classbook out of range"));
        };
        if checked_pow(u64::from(classifications), cb.dimensions)
            .is_none_or(|v| v > u64::from(cb.entries))
        {
            return Err(Error::InvalidData(
                "vorbis: residue classifications^classbook.dimensions exceeds classbook.entries",
            ));
        }

        let mut cascade: Vec<u8> = budget.alloc(classifications as usize)?;
        for slot in &mut cascade {
            let low_bits = r.get(3);
            let bitflag = r.get_bool();
            let high_bits = if bitflag { r.get(5) } else { 0 };
            *slot = (high_bits * 8 + low_bits) as u8;
        }
        let mut books: Vec<[i16; 8]> = budget.alloc(classifications as usize)?;
        for (i, row) in books.iter_mut().enumerate() {
            let cascade_bits = cascade.get(i).copied().unwrap_or(0);
            for (j, slot) in row.iter_mut().enumerate() {
                if cascade_bits & (1 << j) != 0 {
                    let b = r.get(8);
                    let Some(book) = codebooks.get(b as usize) else {
                        return Err(Error::InvalidData("vorbis: residue book out of range"));
                    };
                    if !book.has_lookup() {
                        return Err(Error::InvalidData(
                            "vorbis: residue book has no value mapping",
                        ));
                    }
                    *slot = b as i16;
                } else {
                    *slot = -1;
                }
            }
        }
        if r.overran() {
            return Err(Error::InvalidData("vorbis: eop decoding residue header"));
        }
        Ok(Self {
            residue_type,
            begin,
            end,
            partition_size,
            classifications,
            classbook: u8::try_from(classbook).unwrap_or(u8::MAX),
            books,
        })
    }
}

fn checked_pow(base: u64, exp: u32) -> Option<u64> {
    let mut result: u64 = 1;
    for _ in 0..exp {
        result = result.checked_mul(base)?;
    }
    Some(result)
}

/// Decode `ch` residue vectors of length `n` each (spec sections 8.6.2
/// through 8.6.5). `do_not_decode[j]` marks a vector that must still be
/// allocated and zeroed but is skipped during decode (spec section 8.6.2).
pub(crate) fn decode(
    cfg: &ResidueConfig,
    r: &mut BitReaderLsb<'_>,
    codebooks: &[Codebook],
    ch: usize,
    n: usize,
    do_not_decode: &[bool],
    budget: &mut Budget,
) -> Result<Vec<Vec<f32>>> {
    if cfg.residue_type == 2 {
        return decode_type2(cfg, r, codebooks, ch, n, do_not_decode, budget);
    }
    decode_channels(
        cfg,
        r,
        codebooks,
        cfg.residue_type,
        ch,
        n,
        do_not_decode,
        budget,
    )
}

fn decode_type2(
    cfg: &ResidueConfig,
    r: &mut BitReaderLsb<'_>,
    codebooks: &[Codebook],
    ch: usize,
    n: usize,
    do_not_decode: &[bool],
    budget: &mut Budget,
) -> Result<Vec<Vec<f32>>> {
    let all_skip = (0..ch).all(|j| do_not_decode.get(j).copied().unwrap_or(false));
    let mut out: Vec<Vec<f32>> = Vec::new();
    for _ in 0..ch {
        out.push(budget.alloc(n)?);
    }
    if all_skip || ch == 0 {
        return Ok(out);
    }
    let interleaved_len = n.saturating_mul(ch);
    let single = decode_channels(cfg, r, codebooks, 1, 1, interleaved_len, &[false], budget)?;
    let Some(v) = single.into_iter().next() else {
        return Ok(out);
    };
    for i in 0..n {
        for j in 0..ch {
            if let (Some(dst), Some(&src)) = (
                out.get_mut(j).and_then(|row| row.get_mut(i)),
                v.get(i * ch + j),
            ) {
                *dst = src;
            }
        }
    }
    Ok(out)
}

#[allow(
    clippy::too_many_arguments,
    clippy::integer_division,
    reason = "mirrors the spec's own parameter list; partitions_to_read is spec 8.6.2's own floor division"
)]
fn decode_channels(
    cfg: &ResidueConfig,
    r: &mut BitReaderLsb<'_>,
    codebooks: &[Codebook],
    format: u8,
    ch: usize,
    n: usize,
    do_not_decode: &[bool],
    budget: &mut Budget,
) -> Result<Vec<Vec<f32>>> {
    let mut vectors: Vec<Vec<f32>> = Vec::new();
    for _ in 0..ch {
        vectors.push(budget.alloc(n)?);
    }

    let limit_begin = (cfg.begin as usize).min(n);
    let limit_end = (cfg.end as usize).min(n);
    if limit_end <= limit_begin {
        return Ok(vectors);
    }
    let n_to_read = limit_end - limit_begin;
    let partition_size = (cfg.partition_size as usize).max(1);
    let partitions_to_read = n_to_read / partition_size;
    if partitions_to_read == 0 {
        return Ok(vectors);
    }

    let Some(classbook) = codebooks.get(cfg.classbook as usize) else {
        return Err(Error::InvalidData("vorbis: residue classbook out of range"));
    };
    let classwords_per_codeword = (classbook.dimensions as usize).max(1);
    budget.consume_fuel(
        (partitions_to_read as u64)
            .saturating_mul(ch as u64)
            .saturating_add(64),
    )?;

    let mut classifications: Vec<Vec<u32>> = Vec::new();
    for _ in 0..ch {
        classifications.push(budget.alloc(partitions_to_read)?);
    }

    for pass in 0..8usize {
        let mut partition_count = 0usize;
        while partition_count < partitions_to_read {
            if pass == 0 {
                for j in 0..ch {
                    if do_not_decode.get(j).copied().unwrap_or(false) {
                        continue;
                    }
                    let Some(mut temp) = classbook.decode_scalar(r) else {
                        return Ok(vectors);
                    };
                    for i in (0..classwords_per_codeword).rev() {
                        let idx = partition_count.saturating_add(i);
                        if idx < partitions_to_read
                            && let Some(slot) =
                                classifications.get_mut(j).and_then(|row| row.get_mut(idx))
                        {
                            *slot = temp % cfg.classifications.max(1);
                        }
                        temp /= cfg.classifications.max(1);
                    }
                    if r.overran() {
                        return Ok(vectors);
                    }
                }
            }
            let mut i = 0usize;
            while i < classwords_per_codeword && partition_count < partitions_to_read {
                for j in 0..ch {
                    if do_not_decode.get(j).copied().unwrap_or(false) {
                        continue;
                    }
                    let vqclass = classifications
                        .get(j)
                        .and_then(|row| row.get(partition_count))
                        .copied()
                        .unwrap_or(0);
                    let vqbook_idx = cfg
                        .books
                        .get(vqclass as usize)
                        .and_then(|row| row.get(pass))
                        .copied()
                        .unwrap_or(-1);
                    if vqbook_idx >= 0
                        && let Some(book) = codebooks.get(vqbook_idx as usize)
                    {
                        let offset = limit_begin
                            .saturating_add(partition_count.saturating_mul(partition_size));
                        if let Some(vec_j) = vectors.get_mut(j) {
                            decode_partition(format, book, r, vec_j, offset, partition_size);
                        }
                    }
                }
                if r.overran() {
                    return Ok(vectors);
                }
                i += 1;
                partition_count += 1;
            }
        }
    }
    Ok(vectors)
}

/// Format 0 (spec 8.6.3, interleaved) or format 1 (8.6.4, sequential)
/// partition decode, accumulating into `v[offset..offset+n)`.
#[allow(
    clippy::integer_division,
    reason = "spec 8.6.3's own step = partition_size / codebook_dimensions"
)]
fn decode_partition(
    format: u8,
    book: &Codebook,
    r: &mut BitReaderLsb<'_>,
    v: &mut [f32],
    offset: usize,
    n: usize,
) {
    let dims = (book.dimensions as usize).max(1);
    if format == 0 {
        let step = n / dims;
        if step == 0 {
            return;
        }
        for i in 0..step {
            let Some(entry_temp) = book.decode_vector(r) else {
                return;
            };
            for (j, &val) in entry_temp.iter().enumerate() {
                if let Some(slot) = v.get_mut(
                    offset
                        .saturating_add(i)
                        .saturating_add(j.saturating_mul(step)),
                ) {
                    *slot += val;
                }
            }
            if r.overran() {
                return;
            }
        }
    } else {
        let mut i = 0usize;
        while i < n {
            let Some(entry_temp) = book.decode_vector(r) else {
                return;
            };
            for &val in entry_temp {
                if let Some(slot) = v.get_mut(offset.saturating_add(i)) {
                    *slot += val;
                }
                i += 1;
                if i >= n {
                    break;
                }
            }
            if r.overran() {
                return;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    #[test]
    fn empty_range_produces_zero_vectors_without_reading() {
        let cfg = ResidueConfig {
            residue_type: 0,
            begin: 5,
            end: 5,
            partition_size: 8,
            classifications: 1,
            classbook: 0,
            books: vec![[-1; 8]],
        };
        let mut r = BitReaderLsb::new(&[]);
        let mut budget = Budget::new(Limits::permissive());
        let out = decode_channels(&cfg, &mut r, &[], 1, 2, 16, &[false], &mut budget).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|v| v.iter().all(|&x| x == 0.0)));
    }
}
