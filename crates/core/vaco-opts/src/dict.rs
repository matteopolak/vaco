//! The ordered string dictionary.
//!
//! # Where this belongs
//!
//! Plan 11 §4.2 places `Dict` in `vaco-core`. That crate is a stub without it,
//! and `OptBase::Dict` cannot be implemented without one, so it lives here for
//! now with the same shape. Delete and re-export when `vaco_core::Dict` lands.

use crate::escape::{self, EscapeError, Mode};

/// Insertion-ordered, case-sensitive, multi-key capable string map.
///
/// Option and metadata maps have single-digit entry counts, so this is a `Vec`
/// with a linear scan. The ordering is load-bearing: muxers depend on it for
/// byte-identical output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dict {
    entries: Vec<(Box<str>, Box<str>)>,
}

/// Lookup and insertion modifiers, mirroring `AV_DICT_*`.
#[derive(Debug, Clone, Copy, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "an interface fact: these mirror the AV_DICT_* bits one for one"
)]
pub struct DictFlags {
    pub match_case: bool,
    pub ignore_suffix: bool,
    pub dont_overwrite: bool,
    pub append: bool,
    pub multikey: bool,
}

impl DictFlags {
    /// Case-sensitive exact matching, overwrite on set. The default everywhere
    /// in the option system.
    #[must_use]
    pub const fn exact() -> Self {
        Self {
            match_case: true,
            ignore_suffix: false,
            dont_overwrite: false,
            append: false,
            multikey: false,
        }
    }
}

fn key_matches(have: &str, want: &str, f: DictFlags) -> bool {
    if f.ignore_suffix {
        if f.match_case {
            have.starts_with(want)
        } else {
            have.len() >= want.len()
                && have
                    .get(..want.len())
                    .is_some_and(|h| h.eq_ignore_ascii_case(want))
        }
    } else if f.match_case {
        have == want
    } else {
        have.eq_ignore_ascii_case(want)
    }
}

impl Dict {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.get_with(key, None, DictFlags::exact())
            .map(|(_, _, v)| v)
    }

    /// Positional lookup, so a multi-key dictionary can be walked.
    #[must_use]
    pub fn get_with(
        &self,
        key: &str,
        prev: Option<usize>,
        f: DictFlags,
    ) -> Option<(usize, &str, &str)> {
        let start = prev.map_or(0, |p| p + 1);
        self.entries
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, (k, _))| key_matches(k, key, f))
            .map(|(i, (k, v))| (i, &**k, &**v))
    }

    pub fn set(&mut self, key: &str, val: &str) {
        self.set_with(key, val, DictFlags::exact());
    }

    pub fn set_with(&mut self, key: &str, val: &str, f: DictFlags) {
        let existing = if f.multikey {
            None
        } else {
            self.entries
                .iter()
                .position(|(k, _)| key_matches(k, key, f))
                .and_then(|pos| self.entries.get_mut(pos))
        };
        if let Some(slot) = existing {
            if f.dont_overwrite {
                return;
            }
            if f.append {
                let mut s = slot.1.to_string();
                s.push_str(val);
                slot.1 = s.into_boxed_str();
            } else {
                slot.1 = val.into();
            }
            return;
        }
        self.entries.push((key.into(), val.into()));
    }

    pub fn remove(&mut self, key: &str) -> Option<Box<str>> {
        let pos = self.entries.iter().position(|(k, _)| &**k == key)?;
        Some(self.entries.remove(pos).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (&**k, &**v))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Parse `k=v:k2=v2` with the given separator sets and the standard
    /// escaping rules.
    ///
    /// # Errors
    ///
    /// [`EscapeError`] when the input is not well formed under the escaping
    /// grammar.
    pub fn parse_string(
        &mut self,
        s: &str,
        kv_sep: &str,
        pairs_sep: &str,
        f: DictFlags,
    ) -> Result<(), EscapeError> {
        if s.is_empty() {
            return Ok(());
        }
        for pair in escape::split_raw(s, pairs_sep)? {
            if pair.is_empty() {
                continue;
            }
            let (k, v) = match escape::split_once_raw(pair, kv_sep)? {
                Some((k, v)) => (escape::unescape(k)?, escape::unescape(v)?),
                None => (escape::unescape(pair)?, String::new()),
            };
            self.set_with(&k, &v, f);
        }
        Ok(())
    }

    /// Render with the given separators, escaping both.
    #[must_use]
    pub fn to_string_with(&self, kv_sep: char, pairs_sep: char) -> String {
        let mut special = String::new();
        special.push(kv_sep);
        special.push(pairs_sep);
        let mut out = String::new();
        for (i, (k, v)) in self.iter().enumerate() {
            if i > 0 {
                out.push(pairs_sep);
            }
            out.push_str(&escape::escape(k, &special, Mode::Auto));
            out.push(kv_sep);
            out.push_str(&escape::escape(v, &special, Mode::Auto));
        }
        out
    }
}

impl FromIterator<(String, String)> for Dict {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Self {
            entries: iter
                .into_iter()
                .map(|(k, v)| (k.into_boxed_str(), v.into_boxed_str()))
                .collect(),
        }
    }
}
