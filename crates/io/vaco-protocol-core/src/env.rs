//! The whitelist gate — the security boundary of the I/O layer.
//!
//! # Why this exists
//!
//! A playlist is input. `hls`, `dash`, `concat`, `sdp` and `tee` all open URLs
//! that came out of a file somebody else wrote, and the file gets to choose the
//! scheme. Without a gate, a `.m3u8` served over HTTP can name
//! `file:///etc/passwd` or `http://169.254.169.254/latest/meta-data/` and the
//! demuxer will dutifully fetch it. That is server-side request forgery and
//! arbitrary file read, delivered by a media player.
//!
//! So: **a nested open is not an implementation detail, it is a privilege
//! check**, and [`ProtocolEnv`] is the capability that carries the privilege.
//! It is threaded down through every level and never reconstructed, because a
//! reconstructed environment is a reset privilege check.
//!
//! # The gate
//!
//! ```text
//! allowed(scheme) =
//!       scheme ∉ blacklist                                   (W1)
//!   AND (effective is None OR scheme ∈ effective)            (W2)
//!   AND depth < recursion_limit                              (W4)
//!
//!   effective = whitelist               if the caller gave one   (W3)
//!             = parent.default_whitelist if non-empty
//!             = None
//! ```
//!
//! * **W1** — the blacklist always wins, even over an explicit whitelist entry.
//! * **W2** — a demuxer that opens nested URLs must route through here.
//! * **W3** — an explicit whitelist **replaces** the parent's default grant; it
//!   does not add to it. This used to be a union, which made us strictly more
//!   permissive than the reference: a caller who narrowed the whitelist got
//!   less narrowing than they asked for, in the one mechanism whose entire job
//!   is to stop a playlist from opening `file:///etc/passwd`.
//!
//!   Measured: `-protocol_whitelist tls` alone is refused the nested `tcp:`
//!   open with `Protocol 'tcp' not on whitelist 'tls'!`, even though `tls`
//!   grants `tcp` by default. Found while building `vaco-protocol-tls` and
//!   reported rather than worked around, because this crate was not that
//!   agent's to change.
//!
//!   **The other half is deliberately not implemented.** Whether a *non-empty*
//!   `default_whitelist` should also *restrict* when the caller gives no
//!   explicit list is not something we have measured — the obvious experiment
//!   (an HLS playlist naming a `data:` segment) is refused by a different gate,
//!   `allowed_segment_extensions`, before the whitelist is consulted. Guessing
//!   it would make us *more* restrictive than the reference and break files
//!   that work today, which is the opposite failure and no more acceptable. So
//!   `None` still means unrestricted, and this note is here so the next person
//!   knows it is an open question rather than a settled one.
//! * **W4** — depth increments on every nested open including
//!   protocol-over-protocol, so `cache:async:https://…` is depth 3.

use std::path::Path;
use std::time::Duration;

use vaco_io::CancelToken;

use crate::{DenyReason, ProtocolDesc, ProtocolError, ProtocolRegistry, Result};

/// How deep protocol nesting may go before it is refused.
///
/// Eight is far more than any real URL needs (`cache:async:crypto+https:` is 4)
/// and is small enough that a cycle cannot burn the stack.
pub const DEFAULT_RECURSION_LIMIT: u32 = 8;

/// Everything a nested open needs, passed down and never rebuilt.
#[derive(Debug, Clone, Copy)]
pub struct ProtocolEnv<'a> {
    /// Where schemes are looked up.
    pub registry: &'a ProtocolRegistry,
    /// `-protocol_whitelist`. `None` means unrestricted, which is the right
    /// default for a URL the *user* typed and the wrong one for a URL that came
    /// out of a file.
    pub whitelist: Option<&'a [&'a str]>,
    /// `-protocol_blacklist`. Always wins.
    pub blacklist: Option<&'a [&'a str]>,
    /// The grants of the protocol performing this open. Empty at the top level.
    pub parent_defaults: &'a [&'a str],
    /// How many nested opens deep we already are.
    pub depth: u32,
    /// The cap on `depth`.
    pub recursion_limit: u32,
    /// Rule U2: confine `file` opens to this directory subtree, symlinks
    /// included. `None` means the whole filesystem, which is right only when the
    /// user named the path.
    pub root: Option<&'a Path>,
    /// Cooperative cancellation, checked at every I/O boundary.
    pub cancel: &'a CancelToken,
    /// Per-operation timeout for network transports.
    pub rw_timeout: Option<Duration>,
}

impl<'a> ProtocolEnv<'a> {
    /// A top-level, unrestricted environment: what the CLI uses for a URL the
    /// user typed.
    #[must_use]
    pub const fn new(registry: &'a ProtocolRegistry, cancel: &'a CancelToken) -> Self {
        Self {
            registry,
            whitelist: None,
            blacklist: None,
            parent_defaults: &[],
            depth: 0,
            recursion_limit: DEFAULT_RECURSION_LIMIT,
            root: None,
            cancel,
            rw_timeout: None,
        }
    }

    /// Restrict to `list`.
    #[must_use]
    pub const fn with_whitelist(mut self, list: &'a [&'a str]) -> Self {
        self.whitelist = Some(list);
        self
    }

    /// Refuse everything in `list`, whatever else says.
    #[must_use]
    pub const fn with_blacklist(mut self, list: &'a [&'a str]) -> Self {
        self.blacklist = Some(list);
        self
    }

    /// Confine `file` opens to `root` (rule U2).
    #[must_use]
    pub const fn with_root(mut self, root: &'a Path) -> Self {
        self.root = Some(root);
        self
    }

    /// Lower the nesting cap.
    #[must_use]
    pub const fn with_recursion_limit(mut self, limit: u32) -> Self {
        self.recursion_limit = limit;
        self
    }

    /// Set a per-operation timeout.
    #[must_use]
    pub const fn with_rw_timeout(mut self, t: Duration) -> Self {
        self.rw_timeout = Some(t);
        self
    }

    /// The environment `desc` must use for the URLs *it* opens: one level
    /// deeper, and granting `desc`'s own default whitelist.
    #[must_use]
    pub const fn descend(mut self, desc: &'static ProtocolDesc) -> Self {
        self.depth = self.depth.saturating_add(1);
        self.parent_defaults = desc.default_whitelist;
        self
    }

    /// Apply the gate to `scheme`.
    ///
    /// # Errors
    /// [`ProtocolError::Denied`] with the rule that refused it.
    pub fn check_scheme(&self, scheme: &str) -> Result<()> {
        let deny = |reason| {
            Err(ProtocolError::Denied {
                scheme: scheme.to_owned(),
                reason,
            })
        };

        // W1: the blacklist always wins, and is checked first so that an entry
        // appearing on both lists is refused.
        if let Some(black) = self.blacklist
            && black.iter().any(|s| s.eq_ignore_ascii_case(scheme))
        {
            return deny(DenyReason::Blacklisted);
        }

        // W4: checked before dispatch, so a recursion bomb costs no opens.
        if self.depth >= self.recursion_limit {
            return deny(DenyReason::TooDeep);
        }

        // W2/W3: an explicit whitelist replaces the parent's default grant.
        if let Some(white) = self.whitelist
            && !white.iter().any(|s| s.eq_ignore_ascii_case(scheme))
        {
            return deny(DenyReason::NotWhitelisted);
        }

        Ok(())
    }
}
