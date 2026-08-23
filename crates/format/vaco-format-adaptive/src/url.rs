//! Relative URL resolution against a manifest's own address.
//!
//! Every segment, sub-playlist and MPD `BaseURL` in both formats is
//! ordinarily written relative to the manifest that named it. This is
//! deliberately **not** [`vaco_protocol_core::split_url`] — that function
//! answers "which protocol", not "what is the combined address" — and it is
//! deliberately not a full RFC 3986 implementation, because nothing in the
//! workspace depends on one (D10; no such crate is declared in
//! `[workspace.dependencies]`) and the four cases below are what both
//! `#EXT-X-` URLs and MPD `SegmentTemplate`/`BaseURL` values actually use.
//!
//! # The four cases, in priority order
//!
//! 1. `reference` names its own scheme (`http://…`, `file://…`): used as is.
//! 2. `reference` starts with `//`: takes the base's scheme, keeps everything
//!    else from `reference`.
//! 3. `reference` starts with `/`: absolute path on the base's authority.
//! 4. Otherwise: relative to the base's directory, resolving `.` and `..`
//!    segments the way a filesystem path would.

/// Resolve `reference` against `base`, the manifest's own URL (or local path).
///
/// Total: every input pair produces *some* string, because refusing to
/// combine two strings is worse than combining them wrong in an inspectable
/// way, and whatever comes out still goes through the protocol whitelist
/// before anything is opened.
#[must_use]
pub fn resolve(base: &str, reference: &str) -> String {
    if reference.is_empty() {
        return base.to_owned();
    }
    if has_scheme(reference) {
        return reference.to_owned();
    }
    if let Some(rest) = reference.strip_prefix("//") {
        // Protocol-relative: keep the base's scheme, take everything else.
        let scheme = base.split("://").next().unwrap_or("http");
        return format!("{scheme}://{rest}");
    }
    if reference.starts_with('/') {
        if let Some(authority_end) = authority_end(base) {
            return format!("{}{reference}", &base[..authority_end]);
        }
        return reference.to_owned();
    }
    let dir = directory_of(base);
    normalize(&format!("{dir}{reference}"))
}

/// Whether `s` begins with an RFC-3986-shaped `scheme:`.
///
/// Deliberately excludes a bare Windows drive letter (`C:\...`), which is a
/// single letter followed by `:` and would otherwise be misread as a scheme —
/// the same trap `vaco_protocol_core::split_url` documents (rule S4/U3).
fn has_scheme(s: &str) -> bool {
    let Some(colon) = s.find(':') else {
        return false;
    };
    if colon == 1 {
        return false;
    }
    let Some(scheme) = s.get(..colon) else {
        return false;
    };
    let mut chars = scheme.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Byte offset just past `scheme://authority` in `base`, if it has one.
fn authority_end(base: &str) -> Option<usize> {
    let scheme_end = base.find("://")? + 3;
    let rest = base.get(scheme_end..)?;
    let authority_len = rest.find('/').unwrap_or(rest.len());
    Some(scheme_end + authority_len)
}

/// `base` with everything after the last `/` removed, keeping the slash.
///
/// A `base` with no `/` at all (a bare relative filename) resolves against an
/// empty directory, which is the local-file-in-the-current-directory case.
fn directory_of(base: &str) -> String {
    let search = if let Some(scheme_end) = base.find("://") {
        // Never trim into the scheme/authority: `http://host` has no path
        // slash yet and must not be truncated to `http:/`.
        base.get(scheme_end + 3..).unwrap_or("")
    } else {
        base
    };
    let prefix_len = base.len() - search.len();
    match search.rfind('/') {
        Some(i) => base.get(..prefix_len + i + 1).unwrap_or(base).to_owned(),
        None => base.get(..prefix_len).unwrap_or("").to_owned(),
    }
}

/// Collapse `.` and `..` path segments after the authority, the way a
/// filesystem path would. Leaves everything before the path (`scheme://host`)
/// untouched.
fn normalize(combined: &str) -> String {
    let (prefix, path) = match combined.find("://") {
        Some(scheme_end) => {
            let after_scheme = scheme_end + 3;
            let rest = combined.get(after_scheme..).unwrap_or("");
            match rest.find('/') {
                Some(i) => (
                    combined.get(..after_scheme + i).unwrap_or(""),
                    combined.get(after_scheme + i..).unwrap_or(""),
                ),
                None => return combined.to_owned(),
            }
        }
        None => ("", combined),
    };
    let absolute = path.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if stack.last().is_some_and(|&s| s != "..") {
                    stack.pop();
                } else if !absolute {
                    stack.push("..");
                }
            }
            seg => stack.push(seg),
        }
    }
    let joined = stack.join("/");
    let mut out = String::new();
    out.push_str(prefix);
    if absolute {
        out.push('/');
    }
    out.push_str(&joined);
    if path.ends_with('/') && !out.ends_with('/') {
        out.push('/');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_reference_wins_outright() {
        assert_eq!(
            resolve("http://a/b/master.m3u8", "https://other/x.ts"),
            "https://other/x.ts"
        );
        assert_eq!(resolve("http://a/b/master.m3u8", "file:x.ts"), "file:x.ts");
    }

    #[test]
    fn plain_relative_joins_the_directory() {
        assert_eq!(
            resolve("http://a/b/master.m3u8", "seg1.ts"),
            "http://a/b/seg1.ts"
        );
        assert_eq!(
            resolve("http://a/b/c/master.m3u8", "../seg1.ts"),
            "http://a/b/seg1.ts"
        );
        assert_eq!(
            resolve("http://a/b/master.m3u8", "./low/seg1.ts"),
            "http://a/b/low/seg1.ts"
        );
    }

    #[test]
    fn absolute_path_keeps_the_authority() {
        assert_eq!(
            resolve("http://a/b/master.m3u8", "/x/seg1.ts"),
            "http://a/x/seg1.ts"
        );
    }

    #[test]
    fn protocol_relative_takes_the_base_scheme() {
        assert_eq!(
            resolve("https://a/b/master.m3u8", "//cdn.example/seg1.ts"),
            "https://cdn.example/seg1.ts"
        );
    }

    #[test]
    fn local_paths_have_no_scheme_at_all() {
        assert_eq!(
            resolve("/tmp/hls/master.m3u8", "media_0/seg1.ts"),
            "/tmp/hls/media_0/seg1.ts"
        );
        assert_eq!(resolve("master.m3u8", "seg1.ts"), "seg1.ts");
    }

    #[test]
    fn windows_drive_letter_is_not_mistaken_for_a_scheme() {
        assert!(!has_scheme(r"C:\videos\clip.mkv"));
    }

    #[test]
    fn dot_dot_cannot_escape_above_the_root() {
        assert_eq!(
            resolve("http://a/master.m3u8", "../../../x.ts"),
            "http://a/x.ts"
        );
    }
}
