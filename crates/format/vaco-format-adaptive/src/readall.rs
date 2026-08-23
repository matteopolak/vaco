//! Slurp a whole manifest into memory, under a budget.
//!
//! Unlike the streaming containers the rest of `vaco-format-*` handles, an
//! M3U8 playlist or an MPD is a single small text document that has to be
//! parsed as a whole before anything in it is usable — there is no packet to
//! emit until the whole tag/element set is known. So both `vaco-demux-hls`
//! and `vaco-demux-dash` need exactly this, and it is the one place either
//! reads a genuinely attacker-controlled *length* rather than an
//! attacker-controlled *tag*: a `Content-Length` or a growing pipe could claim
//! anything, and the read must stop well short of exhausting memory.

use vaco_core::{Error, Result};
use vaco_io::MediaSource;
use vaco_limits::Budget;

/// Read every byte `source` has, stopping at `limit` bytes even if the source
/// would offer more.
///
/// Reads in fixed-size chunks rather than trusting [`MediaSource::size`],
/// because a forward-only source (a live playlist fetched from a pipe-backed
/// protocol) may not know its own size at all, and a source that lies about
/// its size is exactly the input this function has to survive.
///
/// # Errors
/// [`Error::LimitExceeded`] once more than `limit` bytes have been read;
/// whatever `source.read` itself reports otherwise.
pub fn read_all_bounded(
    source: &mut dyn MediaSource,
    budget: &mut Budget,
    limit: u64,
) -> Result<Vec<u8>> {
    const CHUNK: usize = 64 * 1024;
    let mut out: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; CHUNK].into_boxed_slice();
    loop {
        let n = source.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let Some(piece) = chunk.get(..n) else {
            break;
        };
        let total = out.len().saturating_add(n) as u64;
        if total > limit {
            return Err(Error::LimitExceeded {
                limit: "adaptive_manifest_bytes",
                requested: total,
                cap: limit,
            });
        }
        budget.check_metadata_bytes(total)?;
        out.extend_from_slice(piece);
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;
    use vaco_limits::Limits;

    #[test]
    fn reads_everything_under_the_limit() {
        let data = b"hello, this is a small playlist".to_vec();
        let mut src = MemorySource::new(data.clone());
        let mut budget = Budget::new(Limits::permissive());
        let out = read_all_bounded(&mut src, &mut budget, 4096).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn refuses_past_the_limit_rather_than_growing_unbounded() {
        let data = vec![b'x'; 1_000_000];
        let mut src = MemorySource::new(data);
        let mut budget = Budget::new(Limits::permissive());
        let err = read_all_bounded(&mut src, &mut budget, 1024).unwrap_err();
        assert!(matches!(err, Error::LimitExceeded { .. }));
    }

    #[test]
    fn a_forward_only_source_is_read_correctly_too() {
        let data = b"m3u8 text over a pipe".to_vec();
        let mut src = MemorySource::forward_only(data.clone());
        let mut budget = Budget::new(Limits::permissive());
        let out = read_all_bounded(&mut src, &mut budget, 4096).unwrap();
        assert_eq!(out, data);
    }
}
