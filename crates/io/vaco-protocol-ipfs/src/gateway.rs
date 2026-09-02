//! Pure gateway-precedence resolution and target-URL construction — no I/O,
//! so this is what the fuzz target and most unit tests exercise directly.
//! Reading the environment and any gateway file is [`crate::protocol`]'s job;
//! this module only decides, given already-fetched candidate strings, which
//! one wins and what URL results.
//!
//! # Measured against `ffmpeg 8.1`, using explicit env vars and a temp
//! `$HOME`/`$IPFS_PATH`
//!
//! Precedence, confirmed both by the reference's own numbered help text
//! ("1. -gateway ... 2. `$IPFS_GATEWAY` ... 3. `$IPFS_PATH` ...") and by its
//! debug log skipping straight to the next source when an earlier one is
//! unset (`$IPFS_GATEWAY is empty.` appears with no attempt to open anything
//! before falling through to `$IPFS_PATH`): `-gateway` option, then
//! `$IPFS_GATEWAY`, then a `gateway` file under `$IPFS_PATH`, then a
//! `gateway` file under `$HOME/.ipfs`. A gateway value's trailing `/` is
//! stripped regardless of which source it came from (measured for the
//! option, the env var, and a gateway file, each with a trailing slash) —
//! consistent with the option's own help text warning against including one.
//!
//! # A genuine reference quirk this crate reproduces
//!
//! The `$IPFS_PATH`-based gateway file path is built by literally
//! concatenating `$IPFS_PATH` with the string `"gateway"`, **no path
//! separator inserted** — confirmed by setting `IPFS_PATH=/tmp/fake_ipfs`
//! (no trailing slash) and getting `The IPFS gateway file (full uri:
//! /tmp/fake_ipfsgateway) doesn't exist` in the reference's own debug log.
//! Only a `$IPFS_PATH` ending in `/` finds its file. The `$HOME/.ipfs`
//! fallback does **not** have this bug: the reference builds that path
//! itself with a separator, and it works with `$HOME` set to a bare
//! directory with no trailing slash. [`ipfs_path_gateway_file`] and
//! [`home_gateway_file`] reproduce this asymmetry exactly rather than
//! "fixing" the first one — D17.

/// Reproduces the reference's literal, separator-free concatenation for the
/// `$IPFS_PATH`-based gateway file. **Deliberately buggy**, matching the
/// reference: a caller-supplied `ipfs_path` without a trailing `/` yields a
/// path that will not exist (e.g. `/home/user/.ipfsgateway`), exactly as
/// measured.
#[must_use]
pub fn ipfs_path_gateway_file(ipfs_path: &str) -> String {
    format!("{ipfs_path}gateway")
}

/// The `$HOME/.ipfs`-based gateway file path — built with a proper
/// separator, unlike [`ipfs_path_gateway_file`]; measured to work with a
/// bare (no trailing slash) `$HOME`.
#[must_use]
pub fn home_gateway_file(home: &str) -> String {
    format!("{home}/.ipfs/gateway")
}

/// Pick the first present, non-empty candidate (after trimming surrounding
/// whitespace — a gateway file typically ends in a newline — and any
/// trailing `/`), in the measured precedence order: `-gateway` option,
/// `$IPFS_GATEWAY`, the `$IPFS_PATH` gateway file's contents, the
/// `$HOME/.ipfs` gateway file's contents.
#[must_use]
pub fn resolve(
    option: &str,
    env_gateway: Option<&str>,
    path_file: Option<&str>,
    home_file: Option<&str>,
) -> Option<String> {
    for raw in [Some(option), env_gateway, path_file, home_file]
        .into_iter()
        .flatten()
    {
        let trimmed = raw.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    None
}

/// Build the HTTP(S) URL the reference actually fetches:
/// `<gateway>/<kind>/<rest-with-any-leading-// stripped>`. `kind` is `"ipfs"`
/// or `"ipns"`. Measured with a raw-byte capture: `ipfs://QmCid/x.mp4`
/// against gateway `http://127.0.0.1:PORT` produces exactly `GET
/// /ipfs/QmCid/x.mp4 HTTP/1.1`.
#[must_use]
pub fn build_target(gateway: &str, kind: &str, rest: &str) -> String {
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    format!("{gateway}/{kind}/{rest}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn option_wins_over_everything_else() {
        assert_eq!(
            resolve(
                "http://opt",
                Some("http://env"),
                Some("http://path"),
                Some("http://home")
            ),
            Some("http://opt".to_owned())
        );
    }

    #[test]
    fn env_wins_when_option_is_unset() {
        assert_eq!(
            resolve(
                "",
                Some("http://env"),
                Some("http://path"),
                Some("http://home")
            ),
            Some("http://env".to_owned())
        );
    }

    #[test]
    fn path_file_wins_over_home_file() {
        assert_eq!(
            resolve("", None, Some("http://path"), Some("http://home")),
            Some("http://path".to_owned())
        );
    }

    #[test]
    fn home_file_is_the_last_resort() {
        assert_eq!(
            resolve("", None, None, Some("http://home")),
            Some("http://home".to_owned())
        );
    }

    #[test]
    fn nothing_configured_resolves_to_none() {
        assert_eq!(resolve("", None, None, None), None);
    }

    #[test]
    fn trailing_slash_is_stripped_regardless_of_source() {
        assert_eq!(
            resolve("http://opt/", None, None, None),
            Some("http://opt".to_owned())
        );
        assert_eq!(
            resolve("", Some("http://env/"), None, None),
            Some("http://env".to_owned())
        );
    }

    #[test]
    fn a_gateway_files_trailing_newline_is_trimmed() {
        assert_eq!(
            resolve("", None, Some("http://path\n"), None),
            Some("http://path".to_owned())
        );
    }

    #[test]
    fn ipfs_path_file_has_the_measured_no_separator_bug() {
        assert_eq!(
            ipfs_path_gateway_file("/tmp/fake_ipfs"),
            "/tmp/fake_ipfsgateway"
        );
        assert_eq!(
            ipfs_path_gateway_file("/tmp/fake_ipfs/"),
            "/tmp/fake_ipfs/gateway"
        );
    }

    #[test]
    fn home_file_always_gets_a_separator() {
        assert_eq!(home_gateway_file("/home/user"), "/home/user/.ipfs/gateway");
    }

    #[test]
    fn build_target_strips_the_authority_style_double_slash() {
        assert_eq!(
            build_target("http://127.0.0.1:9", "ipfs", "//QmCid/x.mp4"),
            "http://127.0.0.1:9/ipfs/QmCid/x.mp4"
        );
    }

    #[test]
    fn build_target_handles_a_bare_cid_with_no_path() {
        assert_eq!(
            build_target("http://gw", "ipfs", "//QmBareCidOnly"),
            "http://gw/ipfs/QmBareCidOnly"
        );
    }

    #[test]
    fn build_target_uses_ipns_for_the_ipns_kind() {
        assert_eq!(
            build_target("http://gw", "ipns", "//example.com/x"),
            "http://gw/ipns/example.com/x"
        );
    }
}
