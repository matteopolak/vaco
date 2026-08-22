//! `Dict`, `escape` and `parse`: the user-facing text grammars.
//!
//! Every parser has a formatter, and the pair is defined by the round trip
//! rather than by inspection. The failure mode this catches is an escaping or
//! formatting corner no hand-written case would think to try.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::float_cmp,
    reason = "test code; assertions are the point and the inputs are known"
)]

use proptest::prelude::*;
use vaco_core::escape::{self, Mode};
use vaco_core::{Dict, DictFlags, Duration, Rational, Rgba, parse};

// ------------------------------------------------------------------ escape

#[test]
fn escaping_levels_nest() {
    // Escaping twice and unescaping twice is the identity — the property the
    // three-level CLI grammar depends on.
    let s = "a:b'c\\d";
    let once = escape::escape(s, ":", Mode::Auto);
    let twice = escape::escape(&once, ":", Mode::Auto);
    assert_eq!(
        escape::unescape(&escape::unescape(&twice).unwrap()).unwrap(),
        s
    );
}

#[test]
fn quoting_handles_the_embedded_quote() {
    assert_eq!(escape::escape("a'b", "", Mode::Quote), "'a'\\''b'");
    assert_eq!(escape::unescape("'a'\\''b'").unwrap(), "a'b");
}

#[test]
fn malformed_escapes_are_errors_not_panics() {
    assert!(escape::unescape("'unterminated").is_err());
    assert!(escape::unescape("trailing\\").is_err());
    assert!(escape::split_raw("'oops", ":").is_err());
    assert!(escape::split_once_raw("bad\\", ":").is_err());
}

#[test]
fn split_ignores_separators_inside_quotes() {
    let parts = escape::split("a:'b:c':d", ':').unwrap();
    assert_eq!(parts, ["a", "b:c", "d"]);
    let parts = escape::split("a:b\\:c:d", ':').unwrap();
    assert_eq!(parts, ["a", "b:c", "d"]);
}

// -------------------------------------------------------------------- dict

#[test]
fn dict_keeps_insertion_order() {
    let mut d = Dict::new();
    d.set("z", "1");
    d.set("a", "2");
    d.set("m", "3");
    assert_eq!(
        d.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        ["z", "a", "m"]
    );
    d.set("a", "9");
    assert_eq!(d.get("a"), Some("9"));
    assert_eq!(d.len(), 3);
}

#[test]
fn dict_flags_do_what_they_say() {
    let mut d = Dict::new();
    d.set("key", "one");

    let mut f = DictFlags::exact();
    f.dont_overwrite = true;
    d.set_with("key", "two", f);
    assert_eq!(d.get("key"), Some("one"));

    let mut f = DictFlags::exact();
    f.append = true;
    d.set_with("key", "+two", f);
    assert_eq!(d.get("key"), Some("one+two"));

    let mut f = DictFlags::exact();
    f.multikey = true;
    d.set_with("key", "three", f);
    assert_eq!(d.len(), 2);
    let (i, _, v) = d.get_with("key", None, DictFlags::exact()).unwrap();
    assert_eq!(v, "one+two");
    assert_eq!(
        d.get_with("key", Some(i), DictFlags::exact()).unwrap().2,
        "three"
    );

    let mut d = Dict::new();
    d.set("KeyName", "v");
    assert_eq!(d.get("keyname"), None);
    assert_eq!(
        d.get_with("keyname", None, DictFlags::default())
            .map(|t| t.2),
        Some("v")
    );
    let mut f = DictFlags::exact();
    f.ignore_suffix = true;
    assert_eq!(d.get_with("Key", None, f).map(|t| t.2), Some("v"));
}

#[test]
fn dict_parses_the_option_string_grammar() {
    let mut d = Dict::new();
    d.parse_string("a=1:b=2:c", "=", ":", DictFlags::exact())
        .unwrap();
    assert_eq!(d.get("a"), Some("1"));
    assert_eq!(d.get("b"), Some("2"));
    assert_eq!(d.get("c"), Some(""));

    let mut d = Dict::new();
    d.parse_string("text='a:b':x=1", "=", ":", DictFlags::exact())
        .unwrap();
    assert_eq!(d.get("text"), Some("a:b"));
    assert_eq!(d.get("x"), Some("1"));
}

// ------------------------------------------------------------------- parse

#[test]
fn image_size_accepts_both_spellings() {
    assert_eq!(parse::image_size("1920x1080"), Some((1920, 1080)));
    assert_eq!(parse::image_size("1920X1080"), Some((1920, 1080)));
    assert_eq!(parse::image_size("hd1080"), Some((1920, 1080)));
    assert_eq!(parse::image_size("qcif"), Some((176, 144)));
    assert_eq!(parse::image_size("uhd4320"), Some((7680, 4320)));
    assert_eq!(parse::image_size("1920"), None);
    assert_eq!(parse::image_size("-1x1"), None);
    assert_eq!(parse::image_size("1920x1080x1"), None);
    assert!(parse::image_size_names().count() >= 53);
}

/// Every case below was probed against the reference (ffmpeg 8.1,
/// `-f rawvideo -video_size <s>`) rather than read off a grammar. See the D17
/// notes on `parse::image_size`.
#[test]
fn image_size_matches_the_reference_strtol_grammar() {
    // The separator is one byte, and it is not required to be an `x`.
    for s in [
        "320x240", "320X240", "320-240", "320 240", "320,240", "320+240",
    ] {
        assert_eq!(parse::image_size(s), Some((320, 240)), "{s:?}");
    }
    // `strtol` skips leading whitespace, but nothing consumes a trailing byte.
    assert_eq!(parse::image_size("  320x240"), Some((320, 240)));
    assert_eq!(parse::image_size("320x 240"), Some((320, 240)));
    assert_eq!(parse::image_size("320x+240"), Some((320, 240)));
    assert_eq!(parse::image_size("320x240 "), None);
    // No separator: the first parse eats the lot and the height comes out 0.
    assert_eq!(parse::image_size("320240"), None);
    assert_eq!(parse::image_size("320x"), None);
    assert_eq!(parse::image_size("x240"), None);
    // Zero and negative dimensions are rejected in either position.
    for s in ["0x1", "0X1", "1x0", "0x0", "-1x2", "-0x240"] {
        assert_eq!(parse::image_size(s), None, "{s:?}");
    }
    // Abbreviations are matched exactly.
    assert_eq!(parse::image_size("vga"), Some((640, 480)));
    assert_eq!(parse::image_size("VGA"), None);
    assert_eq!(parse::image_size(" vga"), None);
}

/// D17: the reference range-checks the `int`-truncated value, so a width just
/// over 2^32 is accepted as a small one. Reproduced deliberately.
#[test]
fn image_size_truncates_before_the_range_check() {
    assert_eq!(
        parse::image_size("2147483647x240"),
        Some((2_147_483_647, 240))
    );
    assert_eq!(parse::image_size("2147483648x240"), None); // truncates to i32::MIN
    assert_eq!(parse::image_size("4294967296x240"), None); // truncates to 0
    assert_eq!(parse::image_size("4294967297x240"), Some((1, 240))); // truncates to 1
    assert_eq!(parse::image_size("8589934593x240"), Some((1, 240))); // 2*2^32 + 1
    // Past LONG_MAX, `strtol` clamps; the clamp truncates to -1.
    assert_eq!(parse::image_size("99999999999999999999x240"), None);
}

/// A multi-byte separator leaves the reference mid-sequence, and the resulting
/// continuation byte is not a digit. We must reject without panicking on the
/// non-`char`-boundary slice.
#[test]
fn image_size_survives_a_multibyte_separator() {
    assert_eq!(parse::image_size("320\u{00d7}240"), None);
    assert_eq!(parse::image_size("320\u{00d7}"), None);
    assert_eq!(parse::image_size("\u{00d7}"), None);
}

#[test]
fn video_rate_abbreviations_are_exact() {
    assert_eq!(parse::video_rate("ntsc"), Some(Rational::new(30000, 1001)));
    assert_eq!(parse::video_rate("pal"), Some(Rational::new(25, 1)));
    assert_eq!(parse::video_rate("film"), Some(Rational::new(24, 1)));
    assert_eq!(
        parse::video_rate("ntsc-film"),
        Some(Rational::new(24000, 1001))
    );
    assert_eq!(
        parse::video_rate("30000/1001"),
        Some(Rational::new(30000, 1001))
    );
    assert_eq!(parse::video_rate("25"), Some(Rational::new(25, 1)));
    // A decimal is approximated, and 29.97 is NOT 30000/1001.
    let r = parse::video_rate("29.97").unwrap();
    assert_eq!(r, Rational::new(2997, 100));
    assert_ne!(r, Rational::new(30000, 1001));
    // 1/0 parses: research 05 §5.6 says filtering it out is the caller's job.
    assert_eq!(parse::rational("1/0"), Some(Rational::new(1, 0)));
    assert_eq!(parse::rational("16:9"), Some(Rational::new(16, 9)));
    assert_eq!(parse::rational("not a rate"), None);
}

#[test]
fn duration_grammar() {
    assert_eq!(
        parse::duration("12:34:56.789"),
        Some(Duration(45_296_789_000))
    );
    assert_eq!(parse::duration("-1:02.5"), Some(Duration(-62_500_000)));
    assert_eq!(parse::duration("1234.5"), Some(Duration(1_234_500_000)));
    assert_eq!(parse::duration("5ms"), Some(Duration(5_000)));
    assert_eq!(parse::duration("2s"), Some(Duration(2_000_000)));
    assert_eq!(parse::duration("7us"), Some(Duration(7)));
    assert_eq!(parse::duration("  3  "), Some(Duration(3_000_000)));
    // Exact decimal scaling: 0.1 s is 100000 us, not 99999.
    assert_eq!(parse::duration("0.1"), Some(Duration(100_000)));
    assert_eq!(parse::duration("0.000001"), Some(Duration(1)));
    assert_eq!(parse::duration("0.0000001"), Some(Duration(0)));
    assert_eq!(parse::duration(""), None);
    assert_eq!(parse::duration("1:2:3:4"), None);
    assert_eq!(parse::duration("::"), None);
    assert_eq!(parse::duration("abc"), None);
    assert_eq!(
        parse::format_duration_clock(Duration(45_296_789_000)),
        "12:34:56.789000"
    );
}

#[test]
fn colour_grammar() {
    assert_eq!(parse::color("red"), Some(Rgba::new(0xff, 0, 0, 0xff)));
    assert_eq!(parse::color("Red"), Some(Rgba::new(0xff, 0, 0, 0xff)));
    assert_eq!(parse::color("#00ff00"), Some(Rgba::new(0, 0xff, 0, 0xff)));
    assert_eq!(
        parse::color("0x0000ff80"),
        Some(Rgba::new(0, 0, 0xff, 0x80))
    );
    assert_eq!(parse::color("red@0.5"), Some(Rgba::new(0xff, 0, 0, 128)));
    assert_eq!(parse::color("red@0x40"), Some(Rgba::new(0xff, 0, 0, 0x40)));
    assert_eq!(
        parse::color("papayawhip"),
        Some(Rgba::new(0xff, 0xef, 0xd5, 0xff))
    );
    // Both spellings of the grey family resolve.
    assert_eq!(parse::color("gray"), parse::color("grey"));
    assert_eq!(
        parse::color("lightslategray"),
        parse::color("lightslategrey")
    );
    assert_eq!(parse::color("nosuchcolour"), None);
    assert_eq!(parse::color("#12345"), None);
    assert_eq!(parse::color("red@2.0"), None);
    // `random` is accepted, opaque, and not a fixed value.
    let a = parse::color("random").unwrap();
    assert_eq!(a.a, 0xff);
    let differs = (0..64).any(|_| parse::color("random").unwrap() != a);
    assert!(differs, "random returned the same colour 64 times");
    // The named table is the full X11/SVG set.
    assert_eq!(parse::color_names().count(), 147);
    assert!(parse::color_names().all(|n| parse::color_by_name(n).is_some()));
}

#[test]
fn boolean_and_binary_spellings() {
    for s in ["1", "true", "on", "yes", "enable", "enabled"] {
        assert_eq!(parse::boolean(s), Some(true), "{s}");
    }
    for s in ["0", "false", "off", "no", "disable", "disabled"] {
        assert_eq!(parse::boolean(s), Some(false), "{s}");
    }
    assert_eq!(parse::boolean("maybe"), None);
    assert_eq!(parse::binary("00ff10"), Some(vec![0x00, 0xff, 0x10]));
    assert_eq!(parse::binary("00FF"), Some(vec![0x00, 0xff]));
    assert_eq!(parse::binary("abc"), None);
    assert_eq!(parse::binary("zz"), None);
}

// ---------------------------------------------------------------- properties

fn any_text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-zA-Z0-9 _.+-]{0,12}",
        3 => "[:=,;\\\\'\\[\\]a-z]{0,12}",
        1 => ".{0,12}",
    ]
}

fn nonempty_key() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-zA-Z0-9_]{1,8}",
        2 => "[:=\\\\'a-z]{1,8}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// `unescape(escape(s))` is the identity, in every mode.
    #[test]
    fn escape_roundtrips(s in any_text(), special in "[:=,;|]{0,3}") {
        for mode in [Mode::Auto, Mode::Backslash, Mode::Quote] {
            let e = escape::escape(&s, &special, mode);
            prop_assert_eq!(escape::unescape(&e).unwrap(), s.clone(), "{:?}", mode);
        }
    }

    /// Escaping is idempotent under nesting: N escapes need exactly N unescapes.
    #[test]
    fn escape_nests(s in any_text(), depth in 1usize..4) {
        let mut e = s.clone();
        for _ in 0..depth {
            e = escape::escape(&e, ":", Mode::Auto);
        }
        for _ in 0..depth {
            e = escape::unescape(&e).unwrap();
        }
        prop_assert_eq!(e, s);
    }

    /// `split(join(parts)) == parts`, whatever the parts contain.
    #[test]
    fn split_inverts_join(parts in prop::collection::vec(any_text(), 1..6)) {
        let sep = ':';
        let joined = parts
            .iter()
            .map(|p| escape::escape(p, ":", Mode::Auto))
            .collect::<Vec<_>>()
            .join(":");
        let got = escape::split(&joined, sep).unwrap();
        prop_assert_eq!(got.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(), parts);
    }

    /// Malformed input is rejected, never a panic and never a hang.
    #[test]
    fn escape_never_panics(s in ".{0,32}") {
        let _ = escape::unescape(&s);
        let _ = escape::split(&s, ':');
        let _ = escape::split_raw(&s, ":=");
        let _ = escape::split_once_raw(&s, ":=");
    }

    /// `Dict` round-trips through its string form.
    #[test]
    fn dict_roundtrips(
        pairs in prop::collection::vec((nonempty_key(), any_text()), 0..6)
    ) {
        // Duplicate keys collapse on the way back in, so the input has to be
        // key-unique for the identity to be stated at all.
        let mut seen = std::collections::HashSet::new();
        let pairs: Vec<_> = pairs.into_iter().filter(|(k, _)| seen.insert(k.clone())).collect();

        let mut d = Dict::new();
        for (k, v) in &pairs {
            d.set(k, v);
        }
        let s = d.to_string_with('=', ':');
        let mut back = Dict::new();
        back.parse_string(&s, "=", ":", DictFlags::exact()).unwrap();
        prop_assert_eq!(&back, &d, "rendered as {:?}", s);
    }

    /// `Dict::parse_string` never panics on arbitrary input.
    #[test]
    fn dict_parse_never_panics(s in ".{0,48}") {
        let mut d = Dict::new();
        let _ = d.parse_string(&s, "=", ":", DictFlags::exact());
    }

    /// Every parser inverts its formatter exactly.
    ///
    /// The size domain is `1..=i32::MAX` because that is the reference's own
    /// accepted range — `format_image_size` will happily render a `u32` the
    /// parser then rejects (0) or aliases onto a different one (> 2^32). See
    /// the D17 notes on `parse::image_size`.
    #[test]
    fn parsers_roundtrip(
        w in 1..=i32::MAX.cast_unsigned(), h in 1..=i32::MAX.cast_unsigned(),
        us in (i64::MIN + 1)..=i64::MAX,
        rgba in any::<(u8, u8, u8, u8)>(),
        num in any::<i32>(), den in any::<i32>(),
        bytes in prop::collection::vec(any::<u8>(), 0..24),
        b in any::<bool>(),
    ) {
        prop_assert_eq!(parse::image_size(&parse::format_image_size(w, h)), Some((w, h)));

        let d = Duration(us);
        prop_assert_eq!(parse::duration(&parse::format_duration(d)), Some(d));

        let c = Rgba::new(rgba.0, rgba.1, rgba.2, rgba.3);
        prop_assert_eq!(parse::color(&parse::format_color(c)), Some(c));
        prop_assert_eq!(parse::color(&c.to_string()), Some(c));

        let r = Rational::new(num, den);
        let back = parse::rational(&parse::format_rational(r)).unwrap();
        prop_assert_eq!((back.num, back.den), (r.num, r.den));

        let hex = parse::format_binary(&bytes);
        let decoded = parse::binary(&hex);
        prop_assert_eq!(decoded.as_deref(), Some(&bytes[..]));
        prop_assert_eq!(parse::boolean(parse::format_boolean(b)), Some(b));
    }

    /// `i64::MIN` microseconds is the one duration that cannot round-trip,
    /// because its magnitude has no positive counterpart. It must still parse
    /// and format without panicking.
    #[test]
    fn duration_extremes(us in prop::sample::select(vec![i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX])) {
        let s = parse::format_duration(Duration(us));
        let back = parse::duration(&s);
        prop_assert!(back == Some(Duration(us)) || us == i64::MIN);
        let _ = parse::format_duration_clock(Duration(us));
    }

    /// No input string panics any parser.
    #[test]
    fn parsers_never_panic(s in ".{0,32}") {
        let _ = parse::image_size(&s);
        let _ = parse::video_rate(&s);
        let _ = parse::rational(&s);
        let _ = parse::duration(&s);
        let _ = parse::color(&s);
        let _ = parse::boolean(&s);
        let _ = parse::binary(&s);
    }

    /// Every abbreviation in every table resolves.
    #[test]
    fn tables_are_self_consistent(i in 0usize..1000) {
        let sizes: Vec<_> = parse::image_size_names().collect();
        let name = sizes[i % sizes.len()];
        prop_assert!(parse::image_size(name).is_some(), "{}", name);
        let rates: Vec<_> = parse::video_rate_names().collect();
        let name = rates[i % rates.len()];
        prop_assert!(parse::video_rate(name).unwrap().is_defined(), "{}", name);
    }
}
