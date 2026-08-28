//! Tying a Composition Playlist to its own package: finding `ASSETMAP.xml`
//! next to it, reading it, and resolving a virtual track's `TrackFileId`s to
//! real paths on disk.
//!
//! # Local files only — deliberately, not by oversight
//!
//! An IMF package (SMPTE ST 2067-2's own definition) is a set of files
//! delivered together; every real-world tool this crate's own spec reading
//! found treats it as local storage, never a streaming source the way a
//! DASH `MPD`/HLS playlist is (`ffmpeg`'s own `imfdec.c` needs the CPL's own
//! local path, per `planning/research/03-libavformat.md`). So this module
//! resolves sibling files with `std::fs` directly, the same choice
//! `vaco-demux-image2::fsutil` already made for its own "a demuxer whose
//! real unit of work is a local file set" case, rather than reaching for
//! `vaco-format-adaptive::RemoteAccess`'s protocol-registry machinery built
//! for HTTP-fetched manifests.
//!
//! That choice has a real, named cost, not a silent one: `Demuxer::bind_url`
//! (`planning/INTERFACE-GAPS.md` gap 7, which explicitly names "MXF
//! OP-Atom" — this crate's own essence — as a case its own `bind_url`
//! substitute had not yet addressed) hands this module only the URL string
//! the caller originally opened the CPL from, with no protocol/whitelist
//! context at all. A CPL genuinely fetched over `http(s)` and then resolved
//! against `std::fs` would defeat W3 (`planning/18-formats.md` §2.3's own
//! rule: a remote manifest's default whitelist excludes `file`) — a
//! malicious remote CPL could name a `TrackFileId` whose ASSETMAP entry
//! points at an absolute local path a whitelist would otherwise refuse.
//! [`Package::for_cpl`] closes that specific hole the cheap way available at
//! this layer: it refuses anything that is not already a plain local path
//! (no `scheme://` prefix) rather than attempting to resolve one. See this
//! crate's top-level docs, "How to change it", for what a full fix would
//! need.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};

use crate::assetmap::{self, AssetMap};
use crate::cpl::Cpl;
use crate::pkl::{self, Pkl};

/// The fixed ASSETMAP filename ST 429-9 defines.
pub const ASSETMAP_FILENAME: &str = "ASSETMAP.xml";

/// A CPL plus everything needed to turn its `TrackFileId`s into real files.
#[derive(Debug)]
pub struct Package {
    pub cpl: Cpl,
    pub assetmap: AssetMap,
    pub pkl: Option<Pkl>,
    base_dir: PathBuf,
}

impl Package {
    /// Attach an already-parsed `cpl` (parsed once, by the caller, from
    /// whatever [`vaco_io::MediaSource`] `open` received it through) to the
    /// package it belongs to: find and parse `ASSETMAP.xml` next to
    /// `cpl_path`, then the Packing List the ASSETMAP itself marks
    /// (best-effort: a missing or unparseable PKL does not stop the
    /// composition from being usable, since nothing here needs the PKL
    /// beyond the integrity metadata `pkl.rs`'s own docs already say this
    /// crate does not verify).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if `cpl_path` names anything other than a
    /// plain local path (see the module docs' W3 account).
    /// [`Error::InvalidData`] for a malformed ASSETMAP, or a
    /// `TrackFileId`/ASSETMAP that plainly does not describe a package
    /// (missing `ASSETMAP.xml`). Propagates whatever reading the file
    /// reports otherwise.
    pub fn for_cpl(cpl: Cpl, cpl_path: &str) -> Result<Self> {
        let path = local_path_only(cpl_path)?;
        let base_dir = path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);

        let assetmap_path = find_assetmap(&base_dir)?;
        let assetmap_bytes = std::fs::read_to_string(&assetmap_path).map_err(Error::Io)?;
        let mut budget = Budget::new(Limits::permissive());
        let assetmap = assetmap::parse(&assetmap_bytes, &mut budget)?;

        let pkl = assetmap
            .entries
            .iter()
            .find(|e| e.is_packing_list)
            .and_then(|e| {
                let pkl_path = base_dir.join(&e.path);
                let bytes = std::fs::read_to_string(&pkl_path).ok()?;
                let mut budget = Budget::new(Limits::permissive());
                pkl::parse(&bytes, &mut budget).ok()
            });

        Ok(Self {
            cpl,
            assetmap,
            pkl,
            base_dir,
        })
    }

    /// Resolve `track_file_id` (a bare, lowercased UUID — see
    /// `assetmap::strip_urn_uuid`) to an absolute path via the ASSETMAP.
    ///
    /// # Errors
    /// [`Error::InvalidData`] when the ASSETMAP names no asset with that
    /// `Id` — a CPL referencing a `TrackFileId` its own package's ASSETMAP
    /// does not list, which is a real, reportable inconsistency, not a
    /// silently-skipped track.
    pub fn resolve_track_file(&self, track_file_id: &str) -> Result<PathBuf> {
        let entry = self.assetmap.resolve(track_file_id).ok_or(Error::InvalidData(
            "imf: CPL names a TrackFileId the ASSETMAP does not list",
        ))?;
        Ok(self.base_dir.join(&entry.path))
    }

    /// Resolve every distinct `TrackFileId` referenced by the CPL's virtual
    /// tracks, up front — so a package missing a referenced file is
    /// reported once, at open time, rather than partway through reading a
    /// stream nobody has gotten to yet.
    ///
    /// # Errors
    /// The first [`Package::resolve_track_file`] failure encountered.
    pub fn resolve_all_track_files(&self) -> Result<HashMap<String, PathBuf>> {
        let mut out = HashMap::new();
        for track in self.cpl.virtual_tracks() {
            for res in &track.resources {
                if !out.contains_key(&res.track_file_id) {
                    let path = self.resolve_track_file(&res.track_file_id)?;
                    out.insert(res.track_file_id.clone(), path);
                }
            }
        }
        Ok(out)
    }
}

/// `ASSETMAP.xml` is expected in the CPL's own directory, case-sensitively
/// per the spec's own literal filename — a fallback case-insensitive scan
/// covers real deliveries this crate has not measured (no reference package
/// was available to check against; see this crate's top-level docs,
/// "Verification") but that other tooling is known to produce with
/// different casing.
fn find_assetmap(dir: &Path) -> Result<PathBuf> {
    let exact = dir.join(ASSETMAP_FILENAME);
    if exact.is_file() {
        return Ok(exact);
    }
    let entries = std::fs::read_dir(dir).map_err(Error::Io)?;
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().eq_ignore_ascii_case(ASSETMAP_FILENAME) {
            return Ok(entry.path());
        }
    }
    Err(Error::InvalidData(
        "imf: no ASSETMAP.xml next to the Composition Playlist",
    ))
}

/// Refuse anything that looks like a URL with a non-`file` scheme — see the
/// module docs' W3 account for why. A bare local path (the overwhelmingly
/// common case: `-i /path/to/CPL_x.xml`, or a relative path from the
/// caller's own working directory) always passes; `scheme://...` never
/// does, matching the shape every real invocation of this crate's other
/// scheme-aware siblings already checks for on the write side of the same
/// distinction.
fn local_path_only(url: &str) -> Result<PathBuf> {
    if let Some(colon) = url.find(':')
        && url[colon + 1..].starts_with("//")
        && url[..colon].chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        && !cfg!(windows)
    {
        return Err(Error::Unsupported(
            "imf: the Composition Playlist must be a local path; a remote CPL cannot safely resolve sibling files (see this crate's docs)",
        ));
    }
    Ok(PathBuf::from(url))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_bare_local_path_is_accepted() {
        assert_eq!(local_path_only("/tmp/x/CPL.xml").unwrap(), PathBuf::from("/tmp/x/CPL.xml"));
        assert_eq!(local_path_only("CPL.xml").unwrap(), PathBuf::from("CPL.xml"));
    }

    #[test]
    fn a_remote_scheme_is_refused() {
        assert!(local_path_only("http://example.com/CPL.xml").is_err());
        assert!(local_path_only("https://example.com/CPL.xml").is_err());
    }

    #[test]
    fn a_windows_drive_letter_is_not_mistaken_for_a_scheme() {
        // `C://foo` is not a real Windows path (that's `C:\foo` or
        // `C:/foo`, one colon no slash-slash) but the parser above is
        // deliberately conservative about what it calls a scheme --
        // checked directly since getting this wrong on a real Windows path
        // would be a much worse failure mode than the security check this
        // guards.
        if cfg!(windows) {
            assert!(local_path_only(r"C:\Users\x\CPL.xml").is_ok());
        }
    }
}
