//! Deciding what one argv entry *is*, before anything is looked up.
//!
//! # Non-UTF-8 arguments
//!
//! Real command lines carry filenames that are not valid UTF-8, and the
//! reference — being C — never notices, passing raw bytes through to `open(2)`
//! and echoing them into diagnostics. Vaco takes the position that:
//!
//! * **File paths and option values stay [`OsString`] and are never
//!   transcoded.** They round-trip byte for byte to the platform's API, which
//!   is the only property that actually matters for them.
//! * **Option names and specifiers must be UTF-8.** They are matched against a
//!   static table of ASCII names and against an ASCII grammar; no valid one can
//!   contain a non-UTF-8 byte. A non-UTF-8 name is therefore unrecognised by
//!   construction, and we say so at the boundary
//!   ([`CliError::NonUtf8OptionName`]) instead of carrying bytes into a lookup
//!   that must fail.
//!
//! The observable difference from the reference is confined to the *rendering*
//! of the name inside the diagnostic: the reference writes the raw bytes, we
//! write `U+FFFD`. [`CliError::raw_operand`] hands the original bytes back so a
//! caller that wants byte-identical stderr can write them itself.
//!
//! Only `OsStr::as_encoded_bytes` is used to inspect bytes — a safe, stable
//! API. Nothing here reconstructs an `OsStr` from bytes, which would need
//! `unsafe` and is forbidden (D2).
//!
//! [`OsString`]: std::ffi::OsString
//! [`CliError::NonUtf8OptionName`]: crate::error::CliError::NonUtf8OptionName
//! [`CliError::raw_operand`]: crate::error::CliError::raw_operand
//!
//! # D16
//!
//! `fd:` and numeric `pipe:<n>` URLs are out of scope project-wide because
//! turning an integer into an owned descriptor needs `FromRawFd`. Nothing here
//! interprets a URL, so the restriction does not bite at this layer — but a
//! caller resolving a group's URL must honour it.

use std::ffi::OsStr;

/// What one argv entry turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token<'a> {
    /// A plain argument: a filename, or an option's value taken positionally.
    Positional(&'a OsStr),
    /// The bare `--` marker: the *next* entry is a positional whatever it looks
    /// like. Not an end-of-options marker — only one entry is affected.
    ForcePositional,
    /// An option name token.
    Option(NameToken<'a>),
    /// Starts with `-` but is not valid UTF-8, so it can never name an option.
    NonUtf8Option(&'a OsStr),
}

/// The decomposition of an option name token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameToken<'a> {
    /// The name with the leading dash removed.
    ///
    /// Exactly **one** dash is removed, always. `--help` therefore yields
    /// `-help`, which is a real entry in the reference's table — that is how
    /// `--help` works, and it is why `--y` does *not* work. There is no general
    /// double-dash prefix rule (verified: `--y` reports
    /// `Unrecognized option '-y'`).
    pub name: &'a str,
    /// `true` when a `/` followed the dash: the value names a file whose
    /// contents are the real value. `-/filter_complex graph.txt`.
    pub file_indirect: bool,
    /// Everything after the first `:` in the name token, verbatim and unparsed.
    ///
    /// `None` when there was no colon at all; `Some("")` for a trailing colon.
    /// The two behave identically for every option the reference has, but the
    /// distinction is kept because it costs nothing and is visible in
    /// diagnostics.
    ///
    /// This is deliberately *not* parsed here. Which grammar applies —
    /// stream specifier, metadata specifier, or "ignored entirely" — depends on
    /// the descriptor, which is not known yet.
    pub spec: Option<&'a str>,
}

impl NameToken<'_> {
    /// The name as the reference prints it in `Unrecognized option '…'` and
    /// `Missing argument for option '…'`: base name plus specifier, without the
    /// dash.
    #[must_use]
    pub fn display_name(&self) -> String {
        match self.spec {
            Some(s) => format!("{}:{}", self.name, s),
            None => self.name.to_owned(),
        }
    }
}

/// Classify one argv entry.
///
/// `forced` short-circuits the whole thing: after a `--`, the next entry is a
/// positional no matter what it looks like.
#[must_use]
pub fn classify(arg: &OsStr, forced: bool) -> Token<'_> {
    if forced {
        return Token::Positional(arg);
    }
    let bytes = arg.as_encoded_bytes();
    // A bare `-` is the conventional stdin/stdout URL, not an option.
    if bytes.first() != Some(&b'-') || bytes.len() < 2 {
        return Token::Positional(arg);
    }
    if bytes == b"--" {
        return Token::ForcePositional;
    }
    let Some(text) = arg.to_str() else {
        return Token::NonUtf8Option(arg);
    };
    let rest = text.get(1..).unwrap_or("");
    let (file_indirect, rest) = match rest.strip_prefix('/') {
        Some(r) => (true, r),
        None => (false, rest),
    };
    // Split at the FIRST colon. No option name contains one, so there is
    // nothing to escape here — the backslash escaping in `m:` lives inside the
    // specifier, which this function does not look at.
    let (name, spec) = match rest.find(':') {
        Some(i) => (
            rest.get(..i).unwrap_or(""),
            Some(rest.get(i + 1..).unwrap_or("")),
        ),
        None => (rest, None),
    };
    Token::Option(NameToken {
        name,
        file_indirect,
        spec,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn opt(s: &str) -> NameToken<'_> {
        match classify(OsStr::new(s), false) {
            Token::Option(n) => n,
            other => panic!("{s:?} should lex as an option, got {other:?}"),
        }
    }

    #[test]
    fn positionals() {
        assert!(matches!(
            classify(OsStr::new("out.mkv"), false),
            Token::Positional(_)
        ));
        // A bare `-` is a URL.
        assert!(matches!(
            classify(OsStr::new("-"), false),
            Token::Positional(_)
        ));
        assert!(matches!(
            classify(OsStr::new(""), false),
            Token::Positional(_)
        ));
        assert_eq!(classify(OsStr::new("--"), false), Token::ForcePositional);
        // Forced: even an option-looking entry is a positional.
        assert!(matches!(
            classify(OsStr::new("-y"), true),
            Token::Positional(_)
        ));
    }

    #[test]
    fn name_and_specifier_split_at_the_first_colon() {
        assert_eq!(opt("-c").name, "c");
        assert_eq!(opt("-c").spec, None);
        assert_eq!(opt("-c:").spec, Some(""));
        assert_eq!(opt("-c:v").spec, Some("v"));
        assert_eq!(opt("-c:v:0").spec, Some("v:0"));
        assert_eq!(opt("-metadata:s:a:1").name, "metadata");
        assert_eq!(opt("-metadata:s:a:1").spec, Some("s:a:1"));
        assert_eq!(opt(r"-c:m\:k").spec, Some(r"m\:k"));
    }

    #[test]
    fn one_dash_is_stripped_not_two() {
        // `--help` reaches the table entry literally named `-help`.
        assert_eq!(opt("--help").name, "-help");
        assert_eq!(opt("--y").name, "-y");
        assert_eq!(opt("-y").name, "y");
    }

    #[test]
    fn file_indirection() {
        assert!(opt("-/filter_complex").file_indirect);
        assert_eq!(opt("-/filter_complex").name, "filter_complex");
        assert!(opt("-/filter:v").file_indirect);
        assert_eq!(opt("-/filter:v").spec, Some("v"));
        assert!(!opt("-filter").file_indirect);
    }

    #[test]
    fn display_name_includes_the_specifier() {
        assert_eq!(opt("-c:v").display_name(), "c:v");
        assert_eq!(opt("-c").display_name(), "c");
        assert_eq!(opt("-c:").display_name(), "c:");
    }

    #[test]
    fn non_utf8_option_names_are_flagged_not_lost() {
        let bad = non_utf8_arg();
        match classify(&bad, false) {
            Token::NonUtf8Option(s) => assert_eq!(s, bad.as_os_str()),
            other => panic!("expected NonUtf8Option, got {other:?}"),
        }
    }

    #[test]
    fn non_utf8_positionals_pass_straight_through() {
        let name = non_utf8_filename();
        match classify(&name, false) {
            Token::Positional(s) => assert_eq!(s, name.as_os_str()),
            other => panic!("expected Positional, got {other:?}"),
        }
    }

    #[cfg(unix)]
    fn non_utf8_arg() -> OsString {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![b'-', 0xff, 0xfe])
    }
    #[cfg(unix)]
    fn non_utf8_filename() -> OsString {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![b'b', 0xff, b'd'])
    }
    // On Windows an `OsString` is WTF-16 and cannot be built from arbitrary
    // bytes without unsafe, so the equivalent case is an unpaired surrogate.
    #[cfg(windows)]
    fn non_utf8_arg() -> OsString {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[u16::from(b'-'), 0xd800])
    }
    #[cfg(windows)]
    fn non_utf8_filename() -> OsString {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[u16::from(b'b'), 0xd800])
    }
}
