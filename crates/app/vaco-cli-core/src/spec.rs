//! Stream specifiers: `v`, `a:1`, `p:1:v:0`, `#0x10`, `m:language:eng`,
//! `disp:default+forced`, `u`.
//!
//! # The grammar, as the reference actually implements it
//!
//! Published documentation describes stream specifiers as a colon-separated
//! list of components. That is close but not right, and the difference decides
//! which command lines work. What the reference runs is a **single-pass token
//! loop over a fixed set of fields**, with three properties no EBNF in the
//! manual captures:
//!
//! 1. **Four tokens are terminal.** After a stream index (`0`), a stream id
//!    (`#1`, `i:1`), a metadata match (`m:k:v`) or `u`, nothing may follow —
//!    not even a colon. `v:0:u` is rejected; `v:u` is fine.
//! 2. **The colon is a separator, not a requirement.** After a non-terminal
//!    token the loop eats one colon if there is one and continues either way.
//!    So `p:1v` ≡ `p:1:v`, and `g:0u` ≡ `g:0:u`.
//! 3. **Two tokens carry a lookahead constraint, and they are different ones.**
//!    A media-type letter is only a media-type letter when the next character is
//!    not alphanumeric (`v-` parses `v` then fails on `-`; `vu` fails whole).
//!    `u` is only `u` when the next character is a colon or the end (`u_` fails
//!    whole). These are not the same rule and they are both observable.
//!
//! Everything in this module was established by probing ffmpeg 8.1; the
//! transcripts are in `docs/app/vaco-cli-core.md`.
//!
//! # Why matching is simple even though parsing is not
//!
//! Because the index token is terminal, it is always **last**. Every other
//! token is a conjunctive predicate whose order cannot matter. So matching is:
//! filter the streams by every predicate present, in container order, then —
//! if an index was given — take the n-th survivor. There is no ordered-narrowing
//! subtlety to get wrong.

use core::fmt;

use vaco_core::MediaType;

use crate::error::SpecError;
use crate::num::{strtol_base0, strtol_index};
use crate::stream::{Disposition, MatchCtx};

/// The media-type letters. `V` is not `MediaType::Video`: it excludes cover art
/// and thumbnail streams, which is why it needs its own variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpecMediaKind {
    /// `v`
    Video,
    /// `V` — video, excluding attached pictures and timed thumbnails.
    VideoNoPic,
    /// `a`
    Audio,
    /// `s`
    Subtitle,
    /// `d`
    Data,
    /// `t`
    Attachment,
}

impl SpecMediaKind {
    #[must_use]
    pub const fn from_letter(c: u8) -> Option<Self> {
        match c {
            b'v' => Some(Self::Video),
            b'V' => Some(Self::VideoNoPic),
            b'a' => Some(Self::Audio),
            b's' => Some(Self::Subtitle),
            b'd' => Some(Self::Data),
            b't' => Some(Self::Attachment),
            _ => None,
        }
    }

    #[must_use]
    pub const fn letter(self) -> char {
        match self {
            Self::Video => 'v',
            Self::VideoNoPic => 'V',
            Self::Audio => 'a',
            Self::Subtitle => 's',
            Self::Data => 'd',
            Self::Attachment => 't',
        }
    }

    /// Whether a stream of this description is selected by this letter.
    #[must_use]
    pub fn matches(self, media: Option<MediaType>, disposition: Disposition) -> bool {
        match self {
            Self::Video => media == Some(MediaType::Video),
            Self::VideoNoPic => {
                media == Some(MediaType::Video)
                    && !disposition
                        .intersects(Disposition::ATTACHED_PIC | Disposition::TIMED_THUMBNAILS)
            }
            Self::Audio => media == Some(MediaType::Audio),
            Self::Subtitle => media == Some(MediaType::Subtitle),
            Self::Data => media == Some(MediaType::Data),
            Self::Attachment => media == Some(MediaType::Attachment),
        }
    }
}

/// How `g:` names its target: by position in the file's group list, or by the
/// group's own id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupRef {
    /// `g:0`
    Index(i64),
    /// `g:#0` or `g:i:0`
    Id(i64),
}

/// A parsed stream specifier.
///
/// An all-`None` specifier is the empty one, which matches every stream —
/// `-c: copy` and `-c copy` behave identically, and both reach here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamSpecifier {
    /// `p:<id>`
    pub program: Option<i64>,
    /// `g:<idx>` / `g:#<id>` / `g:i:<id>`
    pub group: Option<GroupRef>,
    /// `#<id>` / `i:<id>` — terminal.
    pub stream_id: Option<i64>,
    /// One of `v V a s d t`.
    pub media: Option<SpecMediaKind>,
    /// `m:<key>[:<value>]` — terminal. Key matching is case-insensitive; value
    /// matching is case-sensitive. Both verified against the reference.
    pub metadata: Option<(String, Option<String>)>,
    /// `disp:<flags>`
    pub disposition: Option<Disposition>,
    /// `u`
    pub usable: bool,
    /// A trailing integer — terminal, and always last.
    pub index: Option<i64>,
}

/// Whether the parser must consume the whole string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    /// Anything left over is [`SpecError::TrailingGarbage`]. This is what an
    /// `-opt:<spec>` suffix uses.
    Complete,
    /// Stop at the first thing that is not a specifier token and hand the
    /// remainder back. `-map` uses this, then inspects the remainder itself —
    /// which is why its complaint reads "Trailing garbage **after** stream
    /// specifier" while the complete form reads "**at the end of a**".
    Prefix,
}

impl StreamSpecifier {
    /// The specifier that matches everything.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// True when no token at all was given.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Parse a complete specifier.
    ///
    /// # Errors
    /// Any of [`SpecError`]; the text of each is the reference's.
    pub fn parse(s: &str) -> Result<Self, SpecError> {
        let (spec, rest) = Self::parse_prefix(s, ParseMode::Complete)?;
        if rest.is_empty() {
            Ok(spec)
        } else {
            Err(SpecError::TrailingGarbage {
                rest: rest.to_owned(),
            })
        }
    }

    /// Parse as much of `s` as is a specifier, returning the remainder.
    ///
    /// In [`ParseMode::Complete`] the remainder is still returned rather than
    /// rejected; [`StreamSpecifier::parse`] is the wrapper that rejects it. The
    /// mode only affects nothing today and is kept because the reference's two
    /// call sites are genuinely two entry points, and a future divergence
    /// between them should have somewhere to live.
    ///
    /// # Errors
    /// Any of [`SpecError`] except `TrailingGarbage`, which only the complete
    /// form can produce.
    pub fn parse_prefix(s: &str, _mode: ParseMode) -> Result<(Self, &str), SpecError> {
        let mut out = Self::default();
        let mut cur = s;
        // The reference rejects a second program/group designator even when the
        // first was a `#`/`i:` stream id, which is why one flag covers all three.
        let mut program_or_group_seen = false;

        loop {
            let step = Self::token(cur, &mut out, &mut program_or_group_seen)?;
            match step {
                Step::Stop => break,
                Step::Terminal(rest) => {
                    cur = rest;
                    break;
                }
                Step::More(rest) => {
                    cur = rest.strip_prefix(':').unwrap_or(rest);
                }
            }
        }

        Ok((out, cur))
    }

    /// One iteration of the reference's token loop.
    fn token<'a>(
        cur: &'a str,
        out: &mut Self,
        program_or_group_seen: &mut bool,
    ) -> Result<Step<'a>, SpecError> {
        if let Some(rest) = cur.strip_prefix("p:") {
            if *program_or_group_seen {
                return Err(SpecError::MultipleProgramOrGroup);
            }
            let sc = strtol_base0(rest);
            if sc.consumed == 0 {
                return Err(SpecError::ExpectedProgramId {
                    rest: rest.to_owned(),
                });
            }
            out.program = Some(sc.value);
            *program_or_group_seen = true;
            return Ok(Step::More(sc.rest));
        }

        if let Some(rest) = cur.strip_prefix("g:") {
            if *program_or_group_seen {
                return Err(SpecError::MultipleProgramOrGroup);
            }
            let (by_id, digits) = match (rest.strip_prefix('#'), rest.strip_prefix("i:")) {
                (Some(r), _) | (_, Some(r)) => (true, r),
                _ => (false, rest),
            };
            let sc = strtol_base0(digits);
            if sc.consumed == 0 {
                // The reference prints what follows the `#`/`i:` marker, not the
                // whole `g:` payload — verified with `g:i:x` -> "got: x".
                return Err(SpecError::ExpectedGroupRef {
                    rest: digits.to_owned(),
                });
            }
            out.group = Some(if by_id {
                GroupRef::Id(sc.value)
            } else {
                GroupRef::Index(sc.value)
            });
            *program_or_group_seen = true;
            return Ok(Step::More(sc.rest));
        }

        if let Some(rest) = cur.strip_prefix('#').or_else(|| cur.strip_prefix("i:")) {
            if *program_or_group_seen {
                return Err(SpecError::MultipleProgramOrGroup);
            }
            let sc = strtol_base0(rest);
            if sc.consumed == 0 {
                return Err(SpecError::ExpectedStreamId {
                    rest: rest.to_owned(),
                });
            }
            out.stream_id = Some(sc.value);
            return Ok(Step::Terminal(sc.rest));
        }

        if let Some(rest) = cur.strip_prefix("m:") {
            let (key, rest) = scan_escaped(rest);
            let (value, rest) = match rest.strip_prefix(':') {
                Some(after) => {
                    let (v, r) = scan_escaped(after);
                    (Some(v), r)
                }
                None => (None, rest),
            };
            out.metadata = Some((key, value));
            return Ok(Step::Terminal(rest));
        }

        if let Some(rest) = cur.strip_prefix("disp:") {
            if out.disposition.is_some() {
                return Err(SpecError::MultipleDisposition);
            }
            let (token, rest) = scan_disposition(rest);
            out.disposition = Some(parse_disposition(token)?);
            return Ok(Step::More(rest));
        }

        let bytes = cur.as_bytes();
        // `u` is only `u` when a colon or the end follows. `u_`, `ux`, `u ` all
        // fail as a whole, which is how the reference distinguishes the flag
        // from a stray letter.
        if bytes.first() == Some(&b'u') && matches!(bytes.get(1), None | Some(b':')) {
            out.usable = true;
            return Ok(Step::Terminal(cur.get(1..).unwrap_or("")));
        }

        // A media-type letter is only one when the next byte is not
        // alphanumeric. This differs from `u`'s rule, deliberately: `v-` parses
        // the `v` and then trips on `-`, while `vu` never parses at all.
        if let Some(kind) = bytes.first().copied().and_then(SpecMediaKind::from_letter)
            && !bytes.get(1).is_some_and(u8::is_ascii_alphanumeric)
        {
            if out.media.is_some() {
                return Err(SpecError::DuplicateType);
            }
            out.media = Some(kind);
            return Ok(Step::More(cur.get(1..).unwrap_or("")));
        }

        if let Some(sc) = strtol_index(cur) {
            out.index = Some(sc.value);
            return Ok(Step::Terminal(sc.rest));
        }

        Ok(Step::Stop)
    }

    /// The ordered set of matching stream indices, in container order.
    ///
    /// Empty means no match; whether that is fatal is the caller's decision
    /// (`-map` makes it fatal unless the map ends in `?`).
    #[must_use]
    pub fn select(&self, ctx: &MatchCtx<'_>) -> Vec<u32> {
        let program: Option<&[u32]> = self.program.map(|id| {
            ctx.programs
                .iter()
                .find(|p| p.id == id)
                .map_or(&[][..], |p| p.streams.as_slice())
        });
        let group: Option<&[u32]> = self.group.map(|r| match r {
            GroupRef::Index(i) => usize::try_from(i)
                .ok()
                .and_then(|i| ctx.groups.get(i))
                .map_or(&[][..], |g| g.streams.as_slice()),
            GroupRef::Id(id) => ctx
                .groups
                .iter()
                .find(|g| g.id == id)
                .map_or(&[][..], |g| g.streams.as_slice()),
        });

        let mut matched = Vec::new();
        for (pos, s) in ctx.streams.iter().enumerate() {
            let idx = u32::try_from(pos).unwrap_or(u32::MAX);
            if let Some(set) = program
                && !set.contains(&idx)
            {
                continue;
            }
            if let Some(set) = group
                && !set.contains(&idx)
            {
                continue;
            }
            if let Some(kind) = self.media
                && !kind.matches(s.media_type, s.disposition)
            {
                continue;
            }
            if let Some(id) = self.stream_id
                && s.id != id
            {
                continue;
            }
            if let Some(want) = self.disposition
                && !s.disposition.contains(want)
            {
                continue;
            }
            if self.usable && !s.is_usable() {
                continue;
            }
            if let Some((key, value)) = &self.metadata {
                let Some(have) = s.tag(key) else { continue };
                if let Some(want) = value
                    && have != want
                {
                    continue;
                }
            }
            matched.push(idx);
        }

        match self.index {
            // A negative index is unreachable from the grammar (the index token
            // requires a leading digit), but `select` is public and the field is
            // public, so it must still be total.
            Some(n) => usize::try_from(n)
                .ok()
                .and_then(|n| matched.get(n).copied())
                .into_iter()
                .collect(),
            None => matched,
        }
    }

    /// Whether `stream_index` is selected.
    #[must_use]
    pub fn matches(&self, ctx: &MatchCtx<'_>, stream_index: u32) -> bool {
        self.select(ctx).contains(&stream_index)
    }

    /// Render the specifier back to a form that parses to the same thing.
    ///
    /// Not necessarily the text the user wrote: `p:1v` renders as `p:1:v`, and
    /// `0x10` renders as `16`. The invariant is `parse(canonical()) == self`.
    #[must_use]
    pub fn canonical(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(p) = self.program {
            parts.push(format!("p:{p}"));
        }
        match self.group {
            Some(GroupRef::Index(i)) => parts.push(format!("g:{i}")),
            Some(GroupRef::Id(i)) => parts.push(format!("g:i:{i}")),
            None => {}
        }
        if let Some(d) = self.disposition {
            // `disp:0` is legal and means "no bits required", which matches
            // everything — the same as omitting the token, but preserved so the
            // round-trip is exact.
            let names: Vec<&str> = d.names().collect();
            if names.is_empty() {
                parts.push("disp:0".to_owned());
            } else {
                parts.push(format!("disp:{}", names.join("+")));
            }
        }
        if let Some(k) = self.media {
            parts.push(k.letter().to_string());
        }
        // Terminal tokens, at most one of which can be present.
        if let Some(id) = self.stream_id {
            parts.push(format!("i:{id}"));
        } else if let Some((k, v)) = &self.metadata {
            let mut m = format!("m:{}", escape_meta(k));
            if let Some(v) = v {
                m.push(':');
                m.push_str(&escape_meta(v));
            }
            parts.push(m);
        } else if self.usable {
            parts.push("u".to_owned());
        } else if let Some(n) = self.index {
            parts.push(n.to_string());
        }
        parts.join(":")
    }
}

impl fmt::Display for StreamSpecifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical())
    }
}

impl core::str::FromStr for StreamSpecifier {
    type Err = SpecError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

enum Step<'a> {
    /// Nothing matched; the loop ends and `cur` is whatever is left.
    Stop,
    /// A terminal token consumed; nothing may follow.
    Terminal(&'a str),
    /// A non-terminal token consumed; one optional colon then continue.
    More(&'a str),
}

/// Scan a metadata key or value: everything up to the next unescaped `:`, with
/// `\X` yielding a literal `X`.
///
/// A trailing lone backslash consumes nothing further, matching the reference
/// (`-c:m:a\` is accepted and matches the key `a`).
fn scan_escaped(s: &str) -> (String, &str) {
    let mut out = String::new();
    let mut it = s.char_indices();
    let mut end = s.len();
    while let Some((i, c)) = it.next() {
        match c {
            ':' => {
                end = i;
                break;
            }
            '\\' => match it.next() {
                Some((_, next)) => out.push(next),
                None => break,
            },
            _ => out.push(c),
        }
    }
    (out, s.get(end..).unwrap_or(""))
}

/// Re-escape a metadata key or value so it parses back to itself.
fn escape_meta(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == ':' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The disposition token's character class: alphanumerics, `_` and `+`.
///
/// `-` is *not* in it, which is why `disp:default-forced` fails with
/// "Trailing garbage: -forced" rather than being read as a flag subtraction.
fn scan_disposition(s: &str) -> (&str, &str) {
    let end = s
        .bytes()
        .position(|b| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'+'))
        .unwrap_or(s.len());
    (s.get(..end).unwrap_or(""), s.get(end..).unwrap_or(""))
}

/// `default+forced`, or a plain integer.
///
/// The `+` is a *leading* sign on each term, not an infix separator. That is
/// observable: `disp:+default` is accepted while `disp:default+` and `disp:++`
/// are rejected — a naive `split('+')` gets the first of those three wrong.
fn parse_disposition(token: &str) -> Result<Disposition, SpecError> {
    let bad = || SpecError::InvalidDisposition {
        text: token.to_owned(),
    };
    let mut bits = Disposition::NONE;
    let mut rest = token;
    let mut any = false;

    while !rest.is_empty() {
        rest = rest.strip_prefix('+').unwrap_or(rest);
        let end = rest.bytes().position(|b| b == b'+').unwrap_or(rest.len());
        let (term, tail) = (rest.get(..end).unwrap_or(""), rest.get(end..).unwrap_or(""));
        if term.is_empty() {
            return Err(bad());
        }
        if let Some(flag) = Disposition::by_name(term) {
            bits |= flag;
        } else {
            let sc = strtol_base0(term);
            if sc.consumed == 0 || !sc.rest.is_empty() {
                return Err(bad());
            }
            bits |= Disposition::from_bits(sc.value as u32);
        }
        any = true;
        rest = tail;
    }

    if any { Ok(bits) } else { Err(bad()) }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;
    use crate::stream::StreamInfo;

    fn ok(s: &str) -> StreamSpecifier {
        StreamSpecifier::parse(s).unwrap_or_else(|e| panic!("{s:?} should parse, got {e}"))
    }

    fn err(s: &str) -> SpecError {
        match StreamSpecifier::parse(s) {
            Err(e) => e,
            Ok(v) => panic!("{s:?} should not parse, got {v:?}"),
        }
    }

    // ---------------------------------------------------------------- accepted
    // Every string here was accepted by ffmpeg 8.1 via `-c:<spec> copy`.

    #[test]
    fn reference_accepts() {
        for s in [
            "",
            "v",
            "a",
            "s",
            "d",
            "t",
            "V",
            "u",
            "0",
            "1",
            "9",
            "v:0",
            "v:1",
            "a:0",
            "a:2",
            "v:",
            "a:",
            "p:1",
            "p:1:v",
            "p:1:0",
            "p:1u",
            "p:1:u",
            "p:1:v:0",
            "p:1:m:k",
            "p:1:a:0",
            "g:0",
            "g:1",
            "g:#1",
            "g:i:1",
            "g:0:v",
            "g:0:v:0",
            "g:0:m:k",
            "g:0:0",
            "g:0v",
            "g:0u",
            "#1",
            "i:1",
            "i:99",
            "m:key",
            "m:key:value",
            "m:",
            "m::",
            "m:a:",
            "m::b",
            "v:u",
            "V:u",
            "s:u",
            "0x1",
            "010",
            "0x10",
            "#0x10",
            "disp:default",
            "disp:default+forced",
            "disp:0",
            "disp:1",
            "disp:0x1",
            "disp:default:v",
            "disp:default:0",
            "disp:default:u",
            "disp:+default",
            "disp:00",
            "v:disp:default",
            "v:m:k",
            "v:p:1",
            "m:k:u",
            "m:k:v",
            "m:k:0",
            "p:1:disp:default",
            "p:+1",
            "p:-1",
            "p: 1",
            "#+1",
            "#-1",
            "i:-1",
            "g:-1",
            "g:#-1",
            "g:i:-1",
            "v:0x2",
            "99999999999999999999",
            "p:1v",
            "d:0",
            "t:0",
            "V:0",
            "p:0x10:v",
            r"m:a\",
            r"m:a\:b",
            r"m:a\:b:c",
            r"m:a\\b",
        ] {
            assert!(
                StreamSpecifier::parse(s).is_ok(),
                "reference accepts {s:?}, we rejected it: {:?}",
                StreamSpecifier::parse(s)
            );
        }
    }

    // ---------------------------------------------------------------- rejected
    // Every string here was rejected by ffmpeg 8.1, with the message shown.

    #[test]
    fn reference_rejects_with_trailing_garbage() {
        for (s, rest) in [
            ("n", "n"),
            ("x", "x"),
            ("-1", "-1"),
            ("+1", "+1"),
            ("0:v", ":v"),
            ("0:0", ":0"),
            (":", ":"),
            ("::", "::"),
            (":v", ":v"),
            ("a:-1", "-1"),
            (" v", " v"),
            ("v ", " "),
            ("v:0:1", ":1"),
            ("vv", "vv"),
            ("uu", "uu"),
            ("0b1", "b1"),
            ("1e2", "e2"),
            ("v:0u", "u"),
            ("u:", ":"),
            ("u:0", ":0"),
            ("u:v", ":v"),
            ("v:0:u", ":u"),
            ("p:1:v:0u", "u"),
            ("p:1:v:0:u", ":u"),
            ("0:u", ":u"),
            ("#1:u", ":u"),
            ("i:1:u", ":u"),
            ("u:m:k", ":m:k"),
            ("u:#1", ":#1"),
            ("m:k:m:j", ":j"),
            ("#1:#2", ":#2"),
            ("v:0:a:1", ":a:1"),
            ("0:1", ":1"),
            ("m:a:b:c", ":c"),
            ("m:k:v:0", ":0"),
            ("m:k:disp:default", ":default"),
            ("#1u", "u"),
            ("i:1u", "u"),
            ("disp", "disp"),
            ("disp:default-forced", "-forced"),
            ("disp:default,forced", ",forced"),
            ("1_", "_"),
            ("va", "va"),
            ("v0", "v0"),
            ("vx", "vx"),
            ("v-", "-"),
            ("v.", "."),
            ("v_", "_"),
            ("u ", "u "),
            ("u.", "u."),
            ("u#", "u#"),
            ("pu", "pu"),
            ("p1", "p1"),
            ("v:+0", "+0"),
            ("p:1 ", " "),
        ] {
            assert_eq!(
                err(s),
                SpecError::TrailingGarbage {
                    rest: rest.to_owned()
                },
                "for {s:?}"
            );
        }
    }

    #[test]
    fn reference_rejects_with_specific_messages() {
        assert_eq!(err("v:v"), SpecError::DuplicateType);
        assert_eq!(err("v:a"), SpecError::DuplicateType);
        assert_eq!(
            err("p:x"),
            SpecError::ExpectedProgramId { rest: "x".into() }
        );
        assert_eq!(
            err("p:"),
            SpecError::ExpectedProgramId {
                rest: String::new()
            }
        );
        assert_eq!(
            err("p:u"),
            SpecError::ExpectedProgramId { rest: "u".into() }
        );
        assert_eq!(err("g:x"), SpecError::ExpectedGroupRef { rest: "x".into() });
        assert_eq!(
            err("g:"),
            SpecError::ExpectedGroupRef {
                rest: String::new()
            }
        );
        assert_eq!(
            err("g:#"),
            SpecError::ExpectedGroupRef {
                rest: String::new()
            }
        );
        assert_eq!(
            err("g:i:"),
            SpecError::ExpectedGroupRef {
                rest: String::new()
            }
        );
        assert_eq!(
            err("g:i:x"),
            SpecError::ExpectedGroupRef { rest: "x".into() }
        );
        assert_eq!(err("i:x"), SpecError::ExpectedStreamId { rest: "x".into() });
        assert_eq!(
            err("i:"),
            SpecError::ExpectedStreamId {
                rest: String::new()
            }
        );
        assert_eq!(
            err("#"),
            SpecError::ExpectedStreamId {
                rest: String::new()
            }
        );
        assert_eq!(err("#x"), SpecError::ExpectedStreamId { rest: "x".into() });
        assert_eq!(err("p:1:#2"), SpecError::MultipleProgramOrGroup);
        assert_eq!(err("p:1:i:2"), SpecError::MultipleProgramOrGroup);
        assert_eq!(err("g:0:p:1"), SpecError::MultipleProgramOrGroup);
        assert_eq!(err("p:1:g:0"), SpecError::MultipleProgramOrGroup);
        assert_eq!(err("g:0:#1"), SpecError::MultipleProgramOrGroup);
        assert_eq!(err("p:1:p:2"), SpecError::MultipleProgramOrGroup);
        assert_eq!(err("g:0:g:1"), SpecError::MultipleProgramOrGroup);
        assert_eq!(
            err("disp:default:disp:forced"),
            SpecError::MultipleDisposition
        );
        for s in ["disp:x", "disp:", "disp:default+", "disp:++", "disp:_"] {
            assert!(
                matches!(err(s), SpecError::InvalidDisposition { .. }),
                "for {s:?}"
            );
        }
    }

    // ------------------------------------------------------------- token shape

    #[test]
    fn numeric_forms_are_strtol_base_zero() {
        assert_eq!(ok("010").index, Some(8));
        assert_eq!(ok("0x10").index, Some(16));
        assert_eq!(ok("a:0x1").index, Some(1));
        assert_eq!(ok("#0x10").stream_id, Some(16));
        assert_eq!(ok("99999999999999999999").index, Some(i64::MAX));
        assert_eq!(ok("p: 1").program, Some(1));
        assert_eq!(ok("p:-1").program, Some(-1));
    }

    #[test]
    fn empty_trailing_colon_is_the_same_as_nothing() {
        assert_eq!(ok("v:"), ok("v"));
        assert_eq!(ok("a:"), ok("a"));
        assert_eq!(ok(""), StreamSpecifier::all());
        assert!(ok("").is_empty());
    }

    #[test]
    fn colon_is_optional_after_a_non_terminal_token() {
        assert_eq!(ok("p:1v"), ok("p:1:v"));
        assert_eq!(ok("g:0u"), ok("g:0:u"));
        assert_eq!(ok("p:1u"), ok("p:1:u"));
    }

    #[test]
    fn metadata_escaping() {
        assert_eq!(ok(r"m:a\:b").metadata, Some(("a:b".into(), None)));
        assert_eq!(
            ok(r"m:a\:b:c").metadata,
            Some(("a:b".into(), Some("c".into())))
        );
        assert_eq!(ok(r"m:a\\b").metadata, Some((r"a\b".into(), None)));
        assert_eq!(ok(r"m:a\").metadata, Some(("a".into(), None)));
        assert_eq!(ok("m:").metadata, Some((String::new(), None)));
        assert_eq!(
            ok("m::").metadata,
            Some((String::new(), Some(String::new())))
        );
        assert_eq!(ok("m::b").metadata, Some((String::new(), Some("b".into()))));
    }

    #[test]
    fn disposition_forms() {
        assert_eq!(ok("disp:default").disposition, Some(Disposition::DEFAULT));
        assert_eq!(
            ok("disp:default+forced").disposition,
            Some(Disposition::DEFAULT | Disposition::FORCED)
        );
        assert_eq!(ok("disp:0").disposition, Some(Disposition::NONE));
        assert_eq!(ok("disp:0x1").disposition, Some(Disposition::DEFAULT));
    }

    // ---------------------------------------------------------------- matching

    fn stream(index: u32, media: MediaType) -> StreamInfo {
        StreamInfo {
            index,
            id: i64::from(index) + 1,
            media_type: Some(media),
            codec_known: true,
            width: 16,
            height: 16,
            sample_rate: 48_000,
            ..StreamInfo::default()
        }
    }

    fn fixture() -> Vec<StreamInfo> {
        vec![
            stream(0, MediaType::Video),
            stream(1, MediaType::Audio),
            stream(2, MediaType::Audio),
            stream(3, MediaType::Subtitle),
        ]
    }

    #[test]
    fn selection_matches_the_reference() {
        let s = fixture();
        let ctx = MatchCtx::streams(&s);
        let sel = |spec: &str| ok(spec).select(&ctx);
        assert_eq!(sel(""), vec![0, 1, 2, 3]);
        assert_eq!(sel("v"), vec![0]);
        assert_eq!(sel("a"), vec![1, 2]);
        assert_eq!(sel("s"), vec![3]);
        assert_eq!(sel("d"), Vec::<u32>::new());
        assert_eq!(sel("0"), vec![0]);
        assert_eq!(sel("2"), vec![2]);
        assert_eq!(sel("9"), Vec::<u32>::new());
        assert_eq!(sel("v:0"), vec![0]);
        assert_eq!(sel("a:0"), vec![1]);
        assert_eq!(sel("a:1"), vec![2]);
        assert_eq!(sel("a:9"), Vec::<u32>::new());
        assert_eq!(sel("v:"), vec![0]);
        assert_eq!(sel("a:"), vec![1, 2]);
        assert_eq!(sel("#2"), vec![1]);
        assert_eq!(sel("i:4"), vec![3]);
        assert_eq!(sel("u"), vec![0, 1, 2, 3]);
    }

    #[test]
    fn index_counts_within_the_filtered_set() {
        let s = fixture();
        let ctx = MatchCtx::streams(&s);
        // `a:1` is the second *audio* stream, i.e. container index 2.
        assert_eq!(ok("a:1").select(&ctx), vec![2]);
        // whereas a bare `1` is container index 1.
        assert_eq!(ok("1").select(&ctx), vec![1]);
    }

    #[test]
    fn video_no_pic_excludes_cover_art() {
        let mut s = fixture();
        if let Some(v) = s.get_mut(0) {
            v.disposition = Disposition::ATTACHED_PIC;
        }
        let ctx = MatchCtx::streams(&s);
        assert_eq!(ok("v").select(&ctx), vec![0]);
        assert_eq!(ok("V").select(&ctx), Vec::<u32>::new());
    }

    #[test]
    fn metadata_key_is_case_insensitive_value_is_not() {
        let mut s = fixture();
        if let Some(v) = s.get_mut(1) {
            v.tags.set("PLAIN", "p");
        }
        let ctx = MatchCtx::streams(&s);
        assert_eq!(ok("m:plain").select(&ctx), vec![1]);
        assert_eq!(ok("m:PLAIN").select(&ctx), vec![1]);
        assert_eq!(ok("m:plain:p").select(&ctx), vec![1]);
        assert_eq!(ok("m:plain:P").select(&ctx), Vec::<u32>::new());
    }

    #[test]
    fn program_and_group_narrow_before_the_index() {
        use crate::stream::{GroupInfo, ProgramInfo};
        let s = fixture();
        let programs = [ProgramInfo {
            id: 1,
            streams: vec![1, 2, 3],
        }];
        let groups = [GroupInfo {
            id: 7,
            streams: vec![2, 3],
        }];
        let ctx = MatchCtx {
            streams: &s,
            programs: &programs,
            groups: &groups,
        };
        assert_eq!(ok("p:1").select(&ctx), vec![1, 2, 3]);
        assert_eq!(ok("p:1:0").select(&ctx), vec![1]);
        assert_eq!(ok("p:1:a:1").select(&ctx), vec![2]);
        assert_eq!(ok("p:2").select(&ctx), Vec::<u32>::new());
        assert_eq!(ok("g:0").select(&ctx), vec![2, 3]);
        assert_eq!(ok("g:i:7").select(&ctx), vec![2, 3]);
        assert_eq!(ok("g:#7:0").select(&ctx), vec![2]);
        assert_eq!(ok("g:1").select(&ctx), Vec::<u32>::new());
    }

    #[test]
    fn canonical_round_trips() {
        for s in [
            "",
            "v",
            "a:1",
            "p:1:v:0",
            "g:0:a",
            "g:i:3",
            "#4",
            "u",
            "m:k",
            "m:k:v",
            "disp:default+forced",
            "disp:0",
            "V:2",
            r"m:a\:b:c",
        ] {
            let parsed = ok(s);
            let round = StreamSpecifier::parse(&parsed.canonical())
                .unwrap_or_else(|e| panic!("{s:?} -> {:?} -> {e}", parsed.canonical()));
            assert_eq!(parsed, round, "for {s:?} via {:?}", parsed.canonical());
        }
    }

    #[test]
    fn prefix_mode_returns_the_remainder() {
        let (spec, rest) = StreamSpecifier::parse_prefix("v:0abc", ParseMode::Prefix)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(spec.media, Some(SpecMediaKind::Video));
        assert_eq!(spec.index, Some(0));
        assert_eq!(rest, "abc");

        let (spec, rest) = StreamSpecifier::parse_prefix(":v", ParseMode::Prefix)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(spec.is_empty());
        assert_eq!(rest, ":v");
    }
}
