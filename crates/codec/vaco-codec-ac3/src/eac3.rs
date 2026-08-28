//! E-AC-3 extensions: substream walking over the independent + dependent
//! syncframe group that makes up one E-AC-3 access unit. ATSC A/52:2018
//! Annex E.
//!
//! # What this module is, honestly
//!
//! Only reachable behind the non-default `patent-unverified-eac3-decode`
//! feature (see this crate's root docs for why: D9 records E-AC-3 decode's
//! patent status as unresolved, not cleared, so this must never reach a
//! default build or a published binary).
//!
//! This parses frame/substream **structure** — enough to know how many
//! syncframes one access unit spans and to walk `bsi()` for each — and
//! deliberately does not attempt AHT (Adaptive Hybrid Transform), spectral
//! extension or enhanced coupling reconstruction. Those are substantially
//! different coding tools from classic AC-3's exponent/bit-allocation/
//! mantissa/IMDCT pipeline (AHT in particular replaces the transform-domain
//! coding entirely for the bins it covers), and implementing them
//! correctly from specification recall alone, with no primary text
//! available to check against, was judged higher-risk than useful within
//! this session — the failure mode of a wrong implementation here is
//! confidently wrong audio, not a decode that visibly fails. Reporting
//! "not implemented" is a truer answer than guessing.
//!
//! [`Eac3Error::NotImplemented`] exists for a caller that goes on to attempt
//! full reconstruction (out of scope for this session — see
//! `requires_unimplemented_tools`'s doc comment for exactly what is and is
//! not checked) to report rather than guess at AHT/spectral-extension
//! content; [`walk_access_unit`] itself only establishes frame boundaries
//! and never returns it.

use vaco_format_ac3::bsi::Bsi;
use vaco_format_ac3::syncinfo::{self, FrameKind, SyncInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eac3Error {
    NotEac3,
    Truncated,
    /// The frame structurally requires AHT or spectral extension — see
    /// module docs for why this is refused rather than guessed at.
    NotImplemented,
}

/// One independent substream plus every dependent substream that follows it
/// before the next independent one, still unsplit into individual syncframe
/// byte ranges — callers needing per-substream boundaries can re-walk with
/// [`vaco_format_ac3::syncinfo::parse`] using [`AccessUnit::frame_count`].
#[derive(Debug, Clone)]
pub struct AccessUnit {
    pub sample_rate: u32,
    pub acmod: u8,
    pub lfeon: bool,
    pub dialnorm: u8,
    pub frame_count: usize,
    pub total_bytes: usize,
}

/// Walk one E-AC-3 access unit starting at `data[0]`: the leading
/// independent substream (`strmtyp == 0`) and every immediately-following
/// dependent substream (`strmtyp == 1`), stopping at the next independent
/// syncframe or end of data.
///
/// # Errors
/// [`Eac3Error::NotEac3`] if the first frame is not E-AC-3;
/// [`Eac3Error::Truncated`] if a frame's header parses but its declared
/// length runs past the end of `data`.
pub fn walk_access_unit(data: &[u8]) -> Result<AccessUnit, Eac3Error> {
    let first: SyncInfo = syncinfo::parse(data).ok_or(Eac3Error::NotEac3)?;
    if first.kind != FrameKind::Eac3 || first.strmtyp != Some(0) {
        return Err(Eac3Error::NotEac3);
    }
    let bsi = Bsi::parse(data, &first).map_err(|_| Eac3Error::Truncated)?;

    let mut pos = 0usize;
    let mut frame_count = 0usize;
    while let Some(info) = data.get(pos..).and_then(syncinfo::parse) {
        if info.kind != FrameKind::Eac3 {
            break;
        }
        if frame_count > 0 && info.strmtyp != Some(1) {
            break; // next independent substream: a new access unit
        }
        if pos.saturating_add(info.frame_size) > data.len() {
            return Err(Eac3Error::Truncated);
        }
        pos = pos.saturating_add(info.frame_size);
        frame_count = frame_count.saturating_add(1);
    }

    Ok(AccessUnit {
        sample_rate: first.sample_rate,
        acmod: bsi.acmod,
        lfeon: bsi.lfeon,
        dialnorm: bsi.dialnorm,
        frame_count,
        total_bytes: pos,
    })
}

/// Whether `bsi()` alone commits this access unit to a coding tool this
/// module refuses to guess at reconstructing.
///
/// Always `false` today: AHT is signalled per audio block (`chahtinu`/
/// `cplahtinu` bits inside `audblk()`, not `bsi()`), so telling it apart
/// needs parsing at least the first block — out of scope for this session
/// (see the module docs). This function exists so that work has an obvious
/// place to land: a caller attempting reconstruction should check the
/// per-block flags once `audblk()` parsing is extended for E-AC-3, and
/// return [`Eac3Error::NotImplemented`] rather than decode AHT-coded bins as
/// if they were classic-AC-3 mantissas.
#[must_use]
pub const fn requires_unimplemented_tools(_bsi: &Bsi) -> bool {
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn eac3_independent_frame() -> Vec<u8> {
        let mut f = vec![0u8; 1792];
        f[0] = 0x0B;
        f[1] = 0x77;
        f[2] = 0x03; // strmtyp=0, substreamid=0
        f[3] = 0x7f;
        f[4] = 0x3f;
        f[5] = 0x87; // bsid=16
        f
    }

    #[test]
    fn a_lone_independent_substream_is_one_frame() {
        let data = eac3_independent_frame();
        let au = walk_access_unit(&data).unwrap();
        assert_eq!(au.frame_count, 1);
        assert_eq!(au.total_bytes, 1792);
        assert_eq!(au.acmod, 7);
        assert!(au.lfeon);
    }

    #[test]
    fn a_classic_ac3_frame_is_refused() {
        let mut f = vec![0u8; 768];
        f[0] = 0x0B;
        f[1] = 0x77;
        f[4] = 20;
        f[5] = 8 << 3;
        f[6] = 2 << 5;
        assert_eq!(walk_access_unit(&f).unwrap_err(), Eac3Error::NotEac3);
    }

    #[test]
    fn never_panics_on_empty_or_truncated_input() {
        assert!(walk_access_unit(&[]).is_err());
        let mut short = eac3_independent_frame();
        short.truncate(100);
        assert_eq!(walk_access_unit(&short).unwrap_err(), Eac3Error::Truncated);
    }
}
