//! Parses the `-ch_layout` spellings accepted by [`layout`]. Input can come
//! from a command line or container metadata; the chlayout fuzz target covers
//! its no-panic requirement.
//!
//! Alternatives are tried in this order:
//!
//! | form | example | result |
//! |---|---|---|
//! | ambisonic | `ambisonic 2+stereo` | [`ChannelOrder::Ambisonic`] |
//! | standard name | `5.1(side)` | native, named |
//! | default for a count | `6c` | native — the first standard layout with 6 channels |
//! | unordered count | `6C`, `6 channels` | [`ChannelOrder::Unspecified`] |
//! | native mask | `0x3f`, `63`, `077` | native |
//! | channel list | `FL+FR`, `FL@Left+FR` | native, custom or ambisonic |
//!
//! Compatibility edges were measured against `FFmpeg` 8.1; transcripts are in
//! `docs/model/vaco-chlayout.md`. Preserve them even where they differ from a
//! conventional grammar:
//!
//! - Number parsing accepts leading whitespace and `+`, but not trailing
//!   whitespace. Counts use base 10 (`010c` is ten); masks and `USR`/`AMBI`
//!   suffixes use base 0 (`010` is the LFE mask and `USR010` is `BC`). Negative
//!   masks fail, while `0xffffffffffffffff` describes 64 channels.
//! - One trailing `+` is ignored; every other empty list element fails.
//! - Labels truncate to 15 bytes; an empty label is absent. Labels keep an
//!   otherwise representable layout custom, except on ambisonic ACN components.
//!
//! [`Label`] owns the truncation behavior.

use crate::{Channel, ChannelEntry, ChannelLayout, ChannelOrder, Label};

/// What C's `isspace` accepts, which is what `strtol` skips. `str::trim_start`
/// would additionally eat Unicode whitespace the reference does not.
const ASCII_SPACE: [char; 6] = [' ', '\t', '\n', '\r', '\x0b', '\x0c'];

/// Parse a layout description. `None` on anything the reference rejects.
pub(crate) fn layout(s: &str) -> Option<ChannelLayout> {
    if let Some(rest) = s.strip_prefix("ambisonic ") {
        return ambisonic(rest);
    }
    if let Some(l) = standard(s) {
        return Some(l);
    }
    if let Some(l) = counted(s) {
        return Some(l);
    }
    if let Some(l) = mask(s) {
        return Some(l);
    }
    channel_list(s)
}

/// `ambisonic <order>` and `ambisonic <order>+<layout>`.
fn ambisonic(rest: &str) -> Option<ChannelLayout> {
    // `strtol` semantics: leading whitespace is skipped, and a string with no
    // digits at all yields 0 with the cursor left where it started — which is
    // why `ambisonic +stereo` is order 0 plus stereo rather than an error.
    let (order, tail) = strtol_prefix(rest, 0);
    let order = order?;

    // Order is read into an `int` and squared into an `int`, so the largest
    // accepted order is the one where `(order + 1)^2` still fits.
    let order = u32::try_from(order).ok()?;
    let channels = (order.checked_add(1)?).checked_mul(order.checked_add(1)?)?;
    if channels > i32::MAX as u32 {
        return None;
    }
    let order = u16::try_from(order).ok()?;

    let extra: Vec<ChannelEntry> = match tail {
        "" => Vec::new(),
        t => {
            let sub = layout(t.strip_prefix('+')?)?;
            // `ambisonic 1+ambisonic 1` is rejected: the tail must not itself be
            // ambisonic. Every other layout is accepted here, including an
            // unspecified one — `ambisonic 1+4 channels` parses and is then
            // structurally invalid, exactly as in the reference.
            if matches!(sub.order, ChannelOrder::Ambisonic { .. }) {
                return None;
            }
            // Labels survive into the extras: `ambisonic 1+FL@x+FR` describes
            // as `ambisonic 1+2 channels (FL@x+FR)`.
            (0..sub.channels)
                .filter_map(|i| Some((sub.channel_at(i)?, sub.label_at(i).copied())))
                .collect()
        }
    };
    ChannelLayout::ambisonic_labelled(order, extra)
}

/// An exact standard-layout name. No trimming, no case folding.
fn standard(s: &str) -> Option<ChannelLayout> {
    ChannelLayout::standard()
        .find(|(name, _)| *name == s)
        .map(|(_, l)| l)
}

/// `<n>c`, `<n>C`, `<n> channels`, and `<n> channels (<list>)`.
fn counted(s: &str) -> Option<ChannelLayout> {
    let (n, tail) = strtol_prefix(s, 10);
    let n = u32::try_from(n?).ok()?;
    if n == 0 {
        return None;
    }
    match tail {
        "c" => ChannelLayout::default_for(n),
        "C" | " channels" => Some(ChannelLayout::unspecified(n)),
        // D17: the *parenthesised* form is laxer about whitespace than the bare
        // one, because it is a separate code path. `2channels (FL+FC)` and
        // `2  channels (FL+FC)` are both accepted, while the bare `4  channels`
        // is not — the bare form matches the literal `" channels"` and nothing
        // else. The count is checked against the list, so `5 channels (FL+FC)`
        // is an error rather than a silently corrected layout.
        rest => {
            let rest = rest
                .trim_start_matches(ASCII_SPACE)
                .strip_prefix("channels")?;
            let rest = rest.trim_start_matches(ASCII_SPACE);
            let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
            let layout = channel_list(inner)?;
            (layout.channels == n).then_some(layout)
        }
    }
}

/// A bare native mask, base 0.
fn mask(s: &str) -> Option<ChannelLayout> {
    let t = s.trim_start_matches(ASCII_SPACE);
    // Unsigned, but the sign is rejected rather than wrapped — see the D17 note.
    let t = t.strip_prefix('+').unwrap_or(t);
    let (digits, radix) = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (hex, 16)
    } else if t.len() > 1 && t.starts_with('0') {
        (t.get(1..)?, 8)
    } else {
        (t, 10)
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let value = u64::from_str_radix(digits, radix).ok()?;
    ChannelLayout::from_mask(value)
}

/// `A+B+C`, each element `NAME` or `NAME@label`.
fn channel_list(s: &str) -> Option<ChannelLayout> {
    let mut list: Vec<ChannelEntry> = Vec::new();
    let mut rest = s;
    loop {
        let (element, tail) = match rest.split_once('+') {
            Some((head, tail)) => (head, Some(tail)),
            None => (rest, None),
        };
        // Everything after the first `@` is the label, so `FL@a@b` is `FL`
        // labelled `a@b`. `Label::new` applies the 15-byte cap and maps an
        // empty label to `None`, which is what makes `FL@+FR` plain `stereo`.
        let (name, label) = element
            .split_once('@')
            .map_or((element, None), |(name, label)| (name, Label::new(label)));
        list.push((channel(name)?, label));

        match tail {
            Some(t) if !t.is_empty() => rest = t,
            // `None` is the end of the string; `Some("")` is exactly one
            // dangling `+` at the very end, which the reference also ignores.
            Some(_) | None => break,
        }
    }
    ChannelLayout::from_channel_list(list)
}

/// One channel name, with surrounding whitespace already tolerated.
pub(crate) fn channel(s: &str) -> Option<Channel> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix("USR") {
        // D17: `USR` checks that the number consumed the whole token, so
        // `USRX` and `USR018` are errors — but a *missing* number is not, and
        // bare `USR` is `USR0`, which is `FL`.
        let (n, tail) = strtol_prefix(rest, 0);
        return match tail {
            "" => Channel::from_id(u32::try_from(n?).ok()?),
            _ => None,
        };
    }
    if let Some(rest) = s.strip_prefix("AMBI") {
        // D17: `AMBI`, unlike `USR`, ignores whatever follows the number. So
        // `AMBI2X` is `AMBI2`, `AMBI018` is `AMBI1` (octal `01`, then the `8`
        // is simply dropped), and `AMBIX` is `AMBI0` — which is why the string
        // `AMBISONIC` parses, as a single ambisonic channel. Only the range
        // check rejects: `AMBI1024` and `AMBI-1` are errors.
        let (n, _ignored) = strtol_prefix(rest, 0);
        let n = u16::try_from(n?).ok()?;
        return (n < 1024).then_some(Channel::Ambisonic(n));
    }
    match s {
        "UNK" => Some(Channel::Unknown),
        "UNSD" => Some(Channel::Unused),
        name => Channel::named().find(|c| c.short_name() == Some(name)),
    }
}

/// C `strtol` over a non-negative value: skip leading whitespace, read digits in
/// `radix` (or infer the radix when it is 0), and hand back the rest.
///
/// Two `strtol` behaviours are load-bearing and neither is obvious:
///
/// * **No digits is not an error.** The value is `0` and the cursor is restored
///   to where it started, *before* any whitespace skipped. `ambisonic +stereo`
///   is order 0 plus stereo, `ambisonic ` alone is order 0, and a bare `USR` is
///   `USR0` — all three depend on this.
/// * **A negative or out-of-range value is** an error here, reported as `None`.
///   The reference converts it and range-checks afterwards; since every id and
///   count in this grammar is non-negative and bounded well below `i64::MAX`,
///   rejecting at the conversion is equivalent and cannot silently saturate.
fn strtol_prefix(s: &str, radix: u32) -> (Option<i64>, &str) {
    let t = s.trim_start_matches(ASCII_SPACE);
    if t.starts_with('-') {
        return (None, s);
    }
    // `strtol` takes an optional sign; only `+` gets past the check above.
    let t = t.strip_prefix('+').unwrap_or(t);
    // Base 0 infers the radix from the prefix. The leading `0` of an octal
    // literal is itself a digit and stays in the string — dropping it would
    // make `0+mono` a failed conversion instead of the value 0 followed by
    // `+mono`, and `ambisonic 0+mono` depends on the difference.
    let (t, radix) = if radix == 0 {
        match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            Some(hex) if hex.starts_with(|c: char| c.is_ascii_hexdigit()) => (hex, 16),
            _ if t.starts_with('0') => (t, 8),
            _ => (t, 10),
        }
    } else {
        (t, radix)
    };
    let end = t
        .bytes()
        .position(|b| !(b as char).is_digit(radix))
        .unwrap_or(t.len());
    if end == 0 {
        // No digits consumed: value 0, cursor restored to the original start.
        return (Some(0), s);
    }
    let (digits, tail) = (t.get(..end), t.get(end..).unwrap_or(""));
    let value = digits.and_then(|d| i64::from_str_radix(d, radix).ok());
    // An out-of-range literal saturates in C; we reject instead, because every
    // caller bounds the value far below `i64::MAX` anyway and a saturated value
    // would be silently wrong rather than loudly absent.
    (value, tail)
}
