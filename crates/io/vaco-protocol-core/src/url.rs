//! The URL grammar.
//!
//! **This is not RFC 3986 and must not be treated as one.** The reference tool's
//! URL space is a superset with several format-specific escapes, and a parser
//! that normalises them away silently changes which file gets opened:
//!
//! ```text
//! concat:file1.ts|file2.ts|file3.ts
//! subfile,,start,1024,end,4096,,:archive.bin
//! crypto+file:secret.bin
//! tee:out1.mkv|[f=mpegts]out2.ts
//! pipe:1
//! data:audio/wav;base64,UklGR...
//! async:http://host/path
//! C:\videos\clip.mkv
//! ```
//!
//! So we split URLs ourselves, into "which protocol, and what is the rest", and
//! leave the rest to the protocol. RFC 3986 parsing happens *inside* the
//! protocols that genuinely speak it (http, ftp, rtsp) and nowhere else.
//!
//! # Grammar
//!
//! | # | Rule |
//! |---|---|
//! | S1 | No `:` before the first `/` — a bare path. Scheme is `None`, which means `file` (rule U1). |
//! | S2 | Scheme name is `[A-Za-z][A-Za-z0-9+.-]*`, terminated by `:` or by `,`. |
//! | S3 | Terminated by `,`: everything up to the next `:` is `args`, the protocol's own private prefix. |
//! | S4 | A one-letter name followed by `:/` or `:\` is a Windows drive letter, not a scheme. |
//! | S5 | A `+` inside the name splits outer from inner: `crypto+file` is `crypto` over `file`. Splits at the first `+` only. |
//! | S6 | Everything after the terminating `:` is `rest`, parsed by the protocol, never here. |
//!
//! The parse is total: every string is a valid URL, because a string that
//! matches nothing is a relative path, and refusing to open a file because its
//! name is strange is worse than opening it.
//!
//! # Invariant
//!
//! `split_url(s).to_string() == s` for **every** `s`. Splitting loses no bytes,
//! so a nested open cannot smuggle a difference between what was checked and
//! what is opened. This is fuzzed.

use vaco_opts::Dict;

/// A split, unparsed URL.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Url {
    /// The outer protocol name. `None` for a bare path, which means `file`.
    pub scheme: Option<String>,
    /// The inner protocol of a `outer+inner:` pair.
    pub nested: Option<String>,
    /// The protocol's private prefix from the `name,a,b,c:` form, including the
    /// leading separator. Empty when the scheme was terminated by `:`.
    pub args: String,
    /// Everything after the terminating `:`. Uninterpreted.
    pub rest: String,
    /// Trailing `key=value` options, moved out of `rest` by
    /// [`Url::take_inline_opts`]. Empty as produced by [`split_url`].
    pub inline_opts: Dict,
}

/// The scheme a bare path resolves to. Rule U1: a bare path is `file` and only
/// ever `file`, so no configuration can make an unqualified name reach the
/// network.
pub const DEFAULT_SCHEME: &str = "file";

impl Url {
    /// The scheme to dispatch on, applying rule U1.
    #[must_use]
    pub fn effective_scheme(&self) -> &str {
        self.scheme.as_deref().unwrap_or(DEFAULT_SCHEME)
    }

    /// The URL a `outer+inner:` pair delegates to, if there is one.
    ///
    /// `crypto+file:secret.bin` yields `file:secret.bin`.
    #[must_use]
    pub fn nested_url(&self) -> Option<String> {
        self.nested
            .as_ref()
            .map(|inner| format!("{inner}{args}:{rest}", args = self.args, rest = self.rest))
    }

    /// Move a trailing whitespace-separated `key=value` run out of `rest` and
    /// into `inline_opts`.
    ///
    /// Only the RTMP family uses this form (`rtmp://host/app/stream live=1`), so
    /// it is opt-in: a path may legitimately contain both spaces and `=`, and
    /// doing this unconditionally in the splitter would rename files. A token is
    /// only taken if there is whitespace in front of it, so `file:my=name.mkv`
    /// is untouched.
    pub fn take_inline_opts(&mut self) {
        let mut tail: Vec<String> = Vec::new();
        loop {
            let trimmed = self.rest.trim_end();
            let Some(sp) = trimmed.rfind(char::is_whitespace) else {
                break;
            };
            let Some(token) = trimmed.get(sp + 1..) else {
                break;
            };
            if !token.contains('=') {
                break;
            }
            tail.push(token.to_owned());
            self.rest.truncate(sp);
            while self.rest.ends_with(char::is_whitespace) {
                self.rest.pop();
            }
        }
        for token in tail.iter().rev() {
            if let Some((k, v)) = token.split_once('=') {
                self.inline_opts.set(k, v);
            }
        }
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.scheme {
            None => f.write_str(&self.rest),
            Some(scheme) => {
                f.write_str(scheme)?;
                if let Some(inner) = &self.nested {
                    write!(f, "+{inner}")?;
                }
                f.write_str(&self.args)?;
                f.write_str(":")?;
                f.write_str(&self.rest)
            }
        }
    }
}

/// Split `s` into protocol and remainder. Total: never fails.
///
/// See the module docs for the rules and the round-trip invariant.
#[must_use]
pub fn split_url(s: &str) -> Url {
    let bare = |s: &str| Url {
        scheme: None,
        nested: None,
        args: String::new(),
        rest: s.to_owned(),
        inline_opts: Dict::new(),
    };

    let bytes = s.as_bytes();

    // S1: a `/` before any `:` means this is a path, not a URL.
    let first_colon = s.find(':');
    let Some(colon) = first_colon else {
        return bare(s);
    };
    if let Some(slash) = s.find('/')
        && slash < colon
    {
        return bare(s);
    }

    // S2: the scheme name.
    let mut end = 0usize;
    while let Some(&b) = bytes.get(end) {
        if b.is_ascii_alphanumeric() || b == b'+' || b == b'.' || b == b'-' {
            end += 1;
        } else {
            break;
        }
    }
    let Some(name) = s.get(..end) else {
        return bare(s);
    };
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return bare(s);
    }

    // S2/S3: what terminates the name?
    let (args_end, sep) = match bytes.get(end) {
        Some(b':') => (end, end),
        Some(b',') => {
            // S3: scan to the `:` that ends the private prefix.
            let Some(rel) = s.get(end..).and_then(|t| t.find(':')) else {
                return bare(s);
            };
            (end, end + rel)
        }
        _ => return bare(s),
    };

    // S4: `C:\clip.mkv` and `C:/clip.mkv` are drive letters.
    if name.len() == 1 && matches!(bytes.get(sep + 1), Some(b'/' | b'\\')) {
        return bare(s);
    }

    let args = s.get(args_end..sep).unwrap_or("").to_owned();
    let rest = s.get(sep + 1..).unwrap_or("").to_owned();

    // S5: `outer+inner`.
    let (scheme, nested) = match name.split_once('+') {
        Some((outer, inner)) => (outer.to_owned(), Some(inner.to_owned())),
        None => (name.to_owned(), None),
    };

    Url {
        scheme: Some(scheme),
        nested,
        args,
        rest,
        inline_opts: Dict::new(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    #[test]
    fn bare_paths_are_file() {
        for s in [
            "clip.mkv",
            "./dir/clip.mkv",
            "/abs/path/clip.mkv",
            "dir/weird:name.mkv",
            "//server/share/clip.mkv",
            "",
        ] {
            let u = split_url(s);
            assert_eq!(u.scheme, None, "{s}");
            assert_eq!(u.effective_scheme(), "file", "{s}");
            assert_eq!(u.rest, s);
        }
    }

    #[test]
    fn windows_drive_letters_are_not_schemes() {
        for s in [r"C:\videos\clip.mkv", "C:/videos/clip.mkv", r"d:\x"] {
            let u = split_url(s);
            assert_eq!(u.scheme, None, "{s}");
            assert_eq!(u.rest, s);
        }
        // Two letters is a scheme again.
        assert_eq!(split_url("ab:/x").scheme.as_deref(), Some("ab"));
    }

    #[test]
    fn simple_schemes() {
        let u = split_url("pipe:1");
        assert_eq!(u.scheme.as_deref(), Some("pipe"));
        assert_eq!(u.rest, "1");

        let u = split_url("http://host/path?a=b");
        assert_eq!(u.scheme.as_deref(), Some("http"));
        assert_eq!(u.rest, "//host/path?a=b");

        let u = split_url("data:audio/wav;base64,UklGRg==");
        assert_eq!(u.scheme.as_deref(), Some("data"));
        assert_eq!(u.rest, "audio/wav;base64,UklGRg==");
    }

    #[test]
    fn nested_scheme_via_plus() {
        let u = split_url("crypto+file:secret.bin");
        assert_eq!(u.scheme.as_deref(), Some("crypto"));
        assert_eq!(u.nested.as_deref(), Some("file"));
        assert_eq!(u.rest, "secret.bin");
        assert_eq!(u.nested_url().as_deref(), Some("file:secret.bin"));

        // Only the first `+` splits.
        let u = split_url("a+b+c:x");
        assert_eq!(u.scheme.as_deref(), Some("a"));
        assert_eq!(u.nested.as_deref(), Some("b+c"));
    }

    #[test]
    fn comma_args_survive() {
        let u = split_url("subfile,,start,1024,end,4096,,:archive.bin");
        assert_eq!(u.scheme.as_deref(), Some("subfile"));
        assert_eq!(u.args, ",,start,1024,end,4096,,");
        assert_eq!(u.rest, "archive.bin");
        assert_eq!(u.to_string(), "subfile,,start,1024,end,4096,,:archive.bin");
    }

    #[test]
    fn concat_and_tee_rests_are_untouched() {
        let u = split_url("concat:a.ts|b.ts|c.ts");
        assert_eq!(u.rest, "a.ts|b.ts|c.ts");
        let u = split_url("tee:out1.mkv|[f=mpegts]out2.ts");
        assert_eq!(u.rest, "out1.mkv|[f=mpegts]out2.ts");
    }

    #[test]
    fn inline_opts_are_opt_in() {
        let mut u = split_url("rtmp://host/app/stream live=1 timeout=5");
        assert!(u.inline_opts.is_empty());
        u.take_inline_opts();
        assert_eq!(u.rest, "//host/app/stream");
        assert_eq!(u.inline_opts.get("live"), Some("1"));
        assert_eq!(u.inline_opts.get("timeout"), Some("5"));

        // A single token that merely contains `=` is a filename, not an option.
        let mut u = split_url("file:my=file.mkv");
        u.take_inline_opts();
        assert!(u.inline_opts.is_empty());
        assert_eq!(u.rest, "my=file.mkv");
    }

    #[test]
    fn round_trip_is_exact() {
        for s in [
            "",
            ":",
            "1a:x",
            "file:",
            "file:x",
            "a:b",
            "+file:x",
            "crypto+:x",
            "concat:a|b",
            "subfile,x:y",
            "sub,",
            r"C:\x",
            "pipe:0",
            "async:http://h/p",
            "cache:async:https://h/p",
        ] {
            assert_eq!(split_url(s).to_string(), s, "round trip failed for {s:?}");
        }
    }
}
