//! URL-to-path resolution, and the root confinement of rule U2.

use std::path::{Component, Path, PathBuf};

use vaco_protocol_core::{DenyReason, ProtocolError, Result, Url};

/// Turn a `file:` URL — or a bare path — into a filesystem path.
///
/// Handles the RFC 8089 `file://` authority so that the three spellings people
/// actually type all work:
///
/// | URL | Path |
/// |---|---|
/// | `clip.mkv` | `clip.mkv` |
/// | `file:clip.mkv` | `clip.mkv` |
/// | `file:/tmp/clip.mkv` | `/tmp/clip.mkv` |
/// | `file:///tmp/clip.mkv` | `/tmp/clip.mkv` |
/// | `file://localhost/tmp/clip.mkv` | `/tmp/clip.mkv` |
/// | `C:\clip.mkv` | `C:\clip.mkv` (rule S4 kept it a path) |
///
/// A non-empty, non-`localhost` authority is refused rather than guessed at: a
/// UNC share is a network open wearing a local scheme, which is exactly the
/// confusion the whitelist exists to prevent.
///
/// Percent-decoding is **not** performed. The reference tool does not decode
/// `file:` paths either, and a decoder here would make `%2e%2e` a traversal
/// primitive.
///
/// # Errors
/// [`ProtocolError::Malformed`] for a remote authority.
pub fn url_to_path(url: &Url) -> Result<PathBuf> {
    let rest = url.rest.as_str();
    if url.scheme.is_none() {
        return Ok(PathBuf::from(rest));
    }
    let Some(after) = rest.strip_prefix("//") else {
        return Ok(PathBuf::from(rest));
    };
    let (authority, path) = match after.find('/') {
        Some(i) => (after.get(..i).unwrap_or(""), after.get(i..).unwrap_or("")),
        None => (after, ""),
    };
    if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        return Err(ProtocolError::Malformed {
            scheme: "file",
            detail: "a file: url may not name a remote host",
        });
    }
    Ok(PathBuf::from(path))
}

/// Enforce rule U2: a `file` open never escapes an explicitly restricted root.
///
/// `root` is supplied by whoever is opening a URL they did not write — a
/// `concat` list file, a local HLS playlist. It is `None` for a path the user
/// typed, because confining that would be a bug, not a feature.
///
/// Confinement is by **canonical** path, so a symlink pointing out of the root
/// is refused rather than followed. When the target does not exist yet (the
/// create case) the deepest existing ancestor is canonicalised instead and the
/// remaining components are checked for `..` by hand.
///
/// # Errors
/// [`ProtocolError::Denied`] with [`DenyReason::OutsideRoot`].
pub fn confine(path: &Path, root: Option<&Path>) -> Result<PathBuf> {
    let Some(root) = root else {
        return Ok(path.to_path_buf());
    };
    let root = root.canonicalize().map_err(|_| ProtocolError::Denied {
        scheme: "file".to_owned(),
        reason: DenyReason::OutsideRoot,
    })?;

    // Anchor a relative path to the root before resolving it, so `../etc` is
    // rejected rather than resolved against the process working directory.
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };

    let (existing, trailing) = deepest_existing(&joined);
    let real = existing.canonicalize().map_err(|_| ProtocolError::Denied {
        scheme: "file".to_owned(),
        reason: DenyReason::OutsideRoot,
    })?;
    if !real.starts_with(&root) {
        return Err(ProtocolError::Denied {
            scheme: "file".to_owned(),
            reason: DenyReason::OutsideRoot,
        });
    }
    // The part that does not exist yet cannot contain a symlink, but it can
    // contain `..`, which would climb back out once created.
    for c in trailing.components() {
        if matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(ProtocolError::Denied {
                scheme: "file".to_owned(),
                reason: DenyReason::OutsideRoot,
            });
        }
    }
    if trailing.as_os_str().is_empty() {
        // `join("")` would append a trailing separator, which turns a regular
        // file into a path the OS refuses to open.
        return Ok(real);
    }
    Ok(real.join(trailing))
}

/// Split `p` into its deepest existing ancestor and the rest.
fn deepest_existing(p: &Path) -> (PathBuf, PathBuf) {
    let mut prefix = p.to_path_buf();
    let mut suffix = PathBuf::new();
    loop {
        if prefix.exists() {
            return (prefix, suffix);
        }
        let Some(name) = prefix.file_name().map(std::ffi::OsString::from) else {
            return (PathBuf::from("."), p.to_path_buf());
        };
        if !prefix.pop() {
            return (PathBuf::from("."), p.to_path_buf());
        }
        suffix = if suffix.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            PathBuf::from(&name).join(&suffix)
        };
    }
}
