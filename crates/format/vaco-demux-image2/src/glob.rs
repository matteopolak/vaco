//! POSIX-style glob matching for `-pattern_type glob`: `*`, `?`, `[...]`.
//!
//! Pure string matching, no filesystem access — [`crate::fsutil`] is what
//! walks a directory and calls [`glob_match`] on each entry.
//!
//! # Scope
//!
//! Measured against ffmpeg 8.1 (`ffmpeg -pattern_type glob -i 'out*.png'`):
//! `*` and literal characters are enough to cover the common case. `?` and
//! bracket expressions (`[abc]`, `[!abc]`/`[^abc]`, `[a-z]`) are the POSIX
//! `fnmatch` vocabulary and are implemented here on the same understanding,
//! but were not separately probed against the reference. Brace expansion
//! (`{a,b}`) and POSIX character classes (`[[:alpha:]]`) are not implemented;
//! a pattern using them is matched literally, which means it usually matches
//! nothing rather than panicking or silently doing the wrong thing.
//!
//! No recursion: `*` is resolved with the standard two-pointer
//! backtracking algorithm (the one behind `fnmatch`/shell globbing), which is
//! O(pattern × name) and cannot blow the stack on adversarial input — the
//! property the fuzz target in `fuzz/fuzz_targets/image2_pipe_framing.rs`
//! leans on for this module's sibling, and worth stating here even though
//! this module is not itself fuzzed (it parses a command-line pattern, not
//! attacker-controlled bytes).

/// One matchable unit of a compiled glob pattern.
#[derive(Debug, Clone)]
enum Atom {
    /// `*`: any run of characters, including empty.
    Star,
    /// `?`: exactly one character.
    Any,
    /// A literal character.
    Literal(char),
    /// `[...]`/`[!...]`: one character from (or, if negated, outside) the set.
    Class { negate: bool, set: Vec<CharSpan> },
}

#[derive(Debug, Clone, Copy)]
enum CharSpan {
    One(char),
    Range(char, char),
}

impl CharSpan {
    fn contains(self, c: char) -> bool {
        match self {
            Self::One(o) => o == c,
            Self::Range(lo, hi) => lo <= c && c <= hi,
        }
    }
}

fn compile(pattern: &str) -> Vec<Atom> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut atoms = Vec::new();
    let mut i = 0usize;
    while let Some(&c) = chars.get(i) {
        match c {
            '*' => {
                atoms.push(Atom::Star);
                i += 1;
            }
            '?' => {
                atoms.push(Atom::Any);
                i += 1;
            }
            '[' => {
                if let Some((atom, next)) = compile_class(&chars, i) {
                    atoms.push(atom);
                    i = next;
                } else {
                    // Unterminated or empty bracket: treat '[' as a literal,
                    // matching common `fnmatch` fallback behaviour.
                    atoms.push(Atom::Literal('['));
                    i += 1;
                }
            }
            other => {
                atoms.push(Atom::Literal(other));
                i += 1;
            }
        }
    }
    atoms
}

/// Compile a `[...]` bracket expression starting at `chars[start] == '['`.
/// Returns the atom and the index just past the closing `]`.
fn compile_class(chars: &[char], start: usize) -> Option<(Atom, usize)> {
    let mut i = start + 1;
    let negate = matches!(chars.get(i), Some('!' | '^'));
    if negate {
        i += 1;
    }
    let set_start = i;
    let mut set = Vec::new();
    // A ']' right after '[' or '[!' is a literal member, not the terminator.
    let mut first = true;
    loop {
        let c = *chars.get(i)?;
        if c == ']' && !first {
            i += 1;
            break;
        }
        first = false;
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|&e| e != ']') {
            let &hi = chars.get(i + 2)?;
            set.push(CharSpan::Range(c, hi));
            i += 3;
        } else {
            set.push(CharSpan::One(c));
            i += 1;
        }
    }
    if i == set_start {
        return None; // empty class, e.g. "[]" with no negation
    }
    Some((Atom::Class { negate, set }, i))
}

/// Whether `name` matches glob `pattern`.
#[must_use]
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let atoms = compile(pattern);
    let text: Vec<char> = name.chars().collect();
    matches(&atoms, &text)
}

fn atom_matches(atom: &Atom, c: char) -> bool {
    match atom {
        Atom::Any => true,
        Atom::Literal(l) => *l == c,
        Atom::Class { negate, set } => set.iter().any(|s| s.contains(c)) != *negate,
        Atom::Star => false, // handled by the caller
    }
}

/// The classic iterative wildcard-matching algorithm: track the most recent
/// `*` and the text position it last tried, and backtrack there on a
/// mismatch instead of recursing.
fn matches(atoms: &[Atom], text: &[char]) -> bool {
    let mut ai = 0usize;
    let mut ti = 0usize;
    let mut star: Option<usize> = None;
    let mut star_text = 0usize;

    loop {
        let a = atoms.get(ai);
        let t = text.get(ti).copied();
        match (a, t) {
            (Some(Atom::Star), _) => {
                star = Some(ai);
                star_text = ti;
                ai += 1;
            }
            (Some(other), Some(c)) if atom_matches(other, c) => {
                ai += 1;
                ti += 1;
            }
            _ => {
                let Some(s) = star else {
                    return t.is_none() && ai == atoms.len();
                };
                ai = s + 1;
                star_text += 1;
                ti = star_text;
                if ti > text.len() {
                    return false;
                }
            }
        }
        if ai == atoms.len() && ti == text.len() {
            return true;
        }
        if ti > text.len() {
            return false;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn star_matches_everything() {
        assert!(glob_match("*", ""));
        assert!(glob_match("*", "anything.png"));
    }

    #[test]
    fn literal_must_match_exactly() {
        assert!(glob_match("out.png", "out.png"));
        assert!(!glob_match("out.png", "out.PNG"));
    }

    #[test]
    fn star_extension() {
        assert!(glob_match("out*.png", "out001.png"));
        assert!(glob_match("out*.png", "out.png"));
        assert!(!glob_match("out*.png", "out001.jpg"));
    }

    #[test]
    fn question_mark_is_exactly_one() {
        assert!(glob_match("out?.png", "out1.png"));
        assert!(!glob_match("out?.png", "out12.png"));
        assert!(!glob_match("out?.png", "out.png"));
    }

    #[test]
    fn bracket_set_and_range() {
        assert!(glob_match("out[0-9].png", "out5.png"));
        assert!(!glob_match("out[0-9].png", "outa.png"));
        assert!(glob_match("out[abc].png", "outb.png"));
    }

    #[test]
    fn negated_bracket_set() {
        assert!(glob_match("out[!0-9].png", "outa.png"));
        assert!(!glob_match("out[!0-9].png", "out5.png"));
    }

    #[test]
    fn multiple_stars() {
        assert!(glob_match("*.*.png", "a.b.png"));
        assert!(glob_match("**.png", "a.png"));
    }

    #[test]
    fn matching_never_panics_on_malformed_class() {
        let _ = glob_match("out[.png", "out[.png");
        let _ = glob_match("[", "[");
        let _ = glob_match("[!]", "x");
        let _ = glob_match("[]", "");
    }

    proptest! {
        /// Matching never panics on arbitrary pattern/name pairs.
        #[test]
        fn never_panics(pattern in ".{0,24}", name in ".{0,24}") {
            let _ = glob_match(&pattern, &name);
        }

        /// A pattern with no glob metacharacters matches iff it equals the name.
        #[test]
        fn literal_pattern_is_exact_equality(s in "[a-zA-Z0-9_]{0,16}", other in "[a-zA-Z0-9_]{0,16}") {
            prop_assert_eq!(glob_match(&s, &other), s == other);
        }

        /// `*` prepended/appended never turns a match into a non-match.
        #[test]
        fn star_is_permissive(s in "[a-zA-Z0-9_]{0,16}") {
            let pattern = format!("*{s}*");
            prop_assert!(glob_match(&pattern, &s));
        }
    }
}
