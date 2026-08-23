//! The filesystem half of `-pattern_type sequence|glob`.
//!
//! Everything in [`crate::pattern`] and [`crate::glob`] is pure string
//! matching. This module is the other half: it actually opens files and
//! lists directories, which is why it is the one module in this crate that
//! does not build usefully for `wasm32-unknown-unknown` — `std::fs` compiles
//! there (so the crate still builds) and every call in this file returns an
//! I/O error at runtime, because there is no filesystem behind
//! `wasm32-unknown-unknown` without a host binding this crate does not
//! assume. See the crate docs for why that split is deliberate rather than a
//! `#[cfg]` two crates would otherwise need to agree on.

use std::fs;
use std::path::{Path, PathBuf};

use vaco_core::{Error, Result};

use crate::glob::glob_match;
use crate::pattern::SequencePattern;

/// Split a pattern into `(directory, filename-or-pattern)`. A pattern with no
/// `/` is relative to `.`.
#[must_use]
pub fn split_dir_and_name(pattern: &str) -> (PathBuf, &str) {
    match pattern.rfind('/') {
        Some(idx) => {
            let dir = pattern.get(..idx).unwrap_or("");
            let name = pattern.get(idx + 1..).unwrap_or("");
            let dir = if dir.is_empty() { "/" } else { dir };
            (PathBuf::from(dir), name)
        }
        None => (PathBuf::from("."), pattern),
    }
}

/// Find the first index in `[start_number, start_number + range - 1]` for
/// which `dir/pattern.format(index)` exists.
///
/// # Errors
/// An [`Error::Io`] with `ErrorKind::NotFound`, carrying the same message
/// shape the reference reports (`ffmpeg -start_number 5 -i 'out%03d.png'`
/// against files starting at `out010.png`, measured):
/// `"Could find no file or sequence with path '<pattern>' and index in the
/// range <lo>-<hi>"`.
pub fn find_sequence_start(
    dir: &Path,
    display_pattern: &str,
    seq: &SequencePattern,
    start_number: i64,
    range: i64,
) -> Result<i64> {
    let range = range.max(1);
    for offset in 0..range {
        let Some(idx) = start_number.checked_add(offset) else {
            break;
        };
        let candidate = dir.join(seq.format(idx));
        if fs::metadata(&candidate).is_ok() {
            return Ok(idx);
        }
    }
    let hi = start_number.saturating_add(range).saturating_sub(1);
    Err(Error::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "Could find no file or sequence with path '{display_pattern}' and index in the range {start_number}-{hi}"
        ),
    )))
}

/// Whether `dir/seq.format(index)` exists, without searching a range. Used
/// once a sequence's start index is known, to decide whether the *next* frame
/// exists (end of sequence) or the read should fail (a genuine gap).
#[must_use]
pub fn sequence_file_exists(dir: &Path, seq: &SequencePattern, index: i64) -> bool {
    fs::metadata(dir.join(seq.format(index))).is_ok()
}

/// Read `dir/seq.format(index)` whole.
///
/// # Errors
/// Propagates the underlying [`std::io::Error`] (not found, permission,
/// etc).
pub fn read_sequence_file(dir: &Path, seq: &SequencePattern, index: i64) -> Result<Vec<u8>> {
    fs::read(dir.join(seq.format(index))).map_err(Error::from)
}

/// List `dir`'s entries whose filename matches `name_pattern`, sorted
/// lexicographically by filename — the same order `glob(3)` returns without
/// `GLOB_NOSORT`.
///
/// Only the filename component is matched against the pattern; a pattern
/// containing `/` beyond the leading directory split is not supported (see
/// the crate docs for the scope this shares with [`crate::glob`]).
///
/// # Errors
/// Propagates [`std::fs::read_dir`]'s failure (missing/unreadable directory).
pub fn glob_list(dir: &Path, name_pattern: &str) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(Error::from)? {
        let entry = entry.map_err(Error::from)?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue; // non-UTF-8 name: cannot match a UTF-8 glob pattern
        };
        if glob_match(name_pattern, name) {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

/// Read a whole file.
///
/// # Errors
/// Propagates the underlying [`std::io::Error`].
pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(Error::from)
}

/// `-ts_from_file`: the file's modification time, as `(unix_seconds,
/// subsec_nanos)`. `None` when the target has no such metadata (already
/// deleted, or — always, since there is no filesystem — on wasm).
#[must_use]
pub fn file_mtime_unix(path: &Path) -> Option<(i64, u32)> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    // time-gate: `Metadata::modified()` returns std's own `SystemTime` —
    // that is the standard library's API shape, not a choice made here —
    // and nothing on this path calls the panicking `SystemTime::now()`; the
    // value comes from file metadata, and this only measures it against the
    // epoch. `vaco-time` exposes `unix_nanos()` for "now" but has no
    // converter for a `SystemTime` obtained elsewhere, so there is no
    // portable alternative today. On a target with no filesystem
    // (`wasm32-unknown-unknown`), `fs::metadata` above already fails before
    // this line is ever reached, which is this crate's documented wasm story.
    let epoch = std::time::SystemTime::UNIX_EPOCH;
    let dur = modified.duration_since(epoch).ok()?;
    i64::try_from(dur.as_secs())
        .ok()
        .map(|secs| (secs, dur.subsec_nanos()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn splits_directory_and_pattern() {
        assert_eq!(
            split_dir_and_name("dir/sub/out%03d.png"),
            (PathBuf::from("dir/sub"), "out%03d.png")
        );
        assert_eq!(
            split_dir_and_name("out%03d.png"),
            (PathBuf::from("."), "out%03d.png")
        );
    }

    #[test]
    fn find_sequence_start_searches_the_declared_range() {
        let dir = std::env::temp_dir().join(format!(
            "vaco-image2-test-{}-{}",
            std::process::id(),
            "find_start"
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let seq = SequencePattern::parse("out%03d.png").unwrap();
        fs::write(dir.join(seq.format(10)), b"x").unwrap();

        assert!(find_sequence_start(&dir, "out%03d.png", &seq, 5, 5).is_err());
        assert_eq!(
            find_sequence_start(&dir, "out%03d.png", &seq, 6, 5).unwrap(),
            10
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn glob_list_matches_and_sorts() {
        let dir = std::env::temp_dir().join(format!(
            "vaco-image2-test-{}-{}",
            std::process::id(),
            "glob_list"
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("c.png"), b"x").unwrap();
        fs::write(dir.join("a.png"), b"x").unwrap();
        fs::write(dir.join("b.txt"), b"x").unwrap();

        let found = glob_list(&dir, "*.png").unwrap();
        let names: Vec<_> = found
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .collect();
        assert_eq!(names, vec!["a.png", "c.png"]);

        let _ = fs::remove_dir_all(&dir);
    }
}
