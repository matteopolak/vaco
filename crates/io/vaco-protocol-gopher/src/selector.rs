//! Selector parsing and the type-character whitelist.

use vaco_core::Error;

/// The three item types this protocol will fetch, measured against
/// `ffmpeg 8.1` by trying every RFC 1436 type character against a local
/// fake server — see the crate docs for the full table.
const SUPPORTED_TYPES: [char; 3] = ['5', '9', 's'];

/// Split `rest` (everything after `gopher:` — a bare path or a `//host...`
/// form, either way starting at the host) into the type character and the
/// selector string to send.
///
/// Algorithm, measured (see the crate docs for the transcript that pins the
/// "one character, not one path segment" rule): after the host[:port] and
/// its terminating `/`, the type is the *first character* of what follows,
/// and everything else up to (but not including) the next `/` is discarded.
/// The selector is the remainder starting at that next `/`, inclusive, or
/// empty if there is none.
///
/// Returns `(type_char, selector)`. `path` here is already just the part
/// after the host's `/` (i.e. `url::split_off_path`'s second element).
#[must_use]
pub fn parse(path: &str) -> Option<(char, String)> {
    let mut chars = path.chars();
    let ty = chars.next()?;
    let rest = chars.as_str();
    let selector = match rest.find('/') {
        Some(i) => rest.get(i..).unwrap_or("").to_owned(),
        None => String::new(),
    };
    Some((ty, selector))
}

/// Split a `gopher:`/`gophers:` URL's `rest` (as [`vaco_protocol_core::Url`]
/// gives it: `//host[:port]/path` or `//host[:port]`) into the host
/// authority and the path (without its leading `/`).
#[must_use]
pub fn split_authority(rest: &str) -> (&str, &str) {
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => (rest, ""),
    }
}

/// Validate `ty` against [`SUPPORTED_TYPES`].
///
/// # Errors
/// [`vaco_core::Error::Option`] naming the type, mirroring the reference's
/// own `Gopher protocol type '<T>' not supported yet!` (measured — this
/// crate reproduces the message shape, including the char, since it is not
/// secret material the way a key would be).
pub fn check_type(ty: char) -> Result<(), Error> {
    if SUPPORTED_TYPES.contains(&ty) {
        Ok(())
    } else {
        Err(Error::Option {
            name: "gopher".to_owned(),
            detail: format!("Gopher protocol type '{ty}' not supported yet!"),
        })
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
    fn type_and_selector_split_on_a_slash_after_the_type() {
        // Measured: gopher://host/5/some/selector -> type '5', selector
        // "/some/selector" (leading slash kept).
        assert_eq!(
            parse("5/some/selector"),
            Some(('5', "/some/selector".to_owned()))
        );
    }

    #[test]
    fn only_the_first_character_is_the_type_even_in_a_multi_char_segment() {
        // Measured: gopher://host/some/selector (no explicit type) sends
        // "/selector", not "ome/selector" — "some" up to the next '/' is
        // discarded wholesale, not kept minus one character.
        assert_eq!(parse("some/selector"), Some(('s', "/selector".to_owned())));
    }

    #[test]
    fn no_following_slash_means_an_empty_selector() {
        assert_eq!(parse("9"), Some(('9', String::new())));
    }

    #[test]
    fn empty_path_has_no_type_at_all() {
        assert_eq!(parse(""), None);
    }

    #[test]
    fn split_authority_handles_host_port_and_bare_host() {
        assert_eq!(split_authority("//host:70/9/x"), ("host:70", "9/x"));
        assert_eq!(split_authority("//host"), ("host", ""));
        assert_eq!(split_authority("//host/"), ("host", ""));
    }

    #[test]
    fn only_5_9_and_s_are_accepted() {
        for ty in ['5', '9', 's'] {
            assert!(check_type(ty).is_ok(), "{ty}");
        }
        for ty in [
            '0', '1', '2', '3', '4', '6', '7', '8', 'g', 'h', 'I', 'i', 'T', 'w',
        ] {
            assert!(check_type(ty).is_err(), "{ty}");
        }
    }

    #[test]
    fn parse_never_panics_on_adversarial_input() {
        for s in ["", "/", "//", "///", "\u{0}", "🦀/x", "5"] {
            let _ = parse(s);
        }
    }
}
