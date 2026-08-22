//! Property tests.
//!
//! The central one is the round trip: `set_from_string(serialize(x)) == x` for
//! an arbitrary instance covering every base, with `skip_defaults` both on and
//! off. Serialisation is *defined* by that identity rather than by inspection,
//! which is why property testing earns its place here — the failure mode this
//! catches is an escaping or formatting corner that no hand-written case would
//! think to try.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::float_cmp
)]

mod support;

use proptest::prelude::*;
use support::{AllKinds, ChildOpts, TFlags, TMethod, TestChLayout, TestPixFmt, TestSampleFmt};
use vaco_core::{Duration, Rational};
use vaco_opts::{
    Binary, Dict, OptError, OptValue, Options, OptionsExt, ParseCtx, Rgba, SerCtx, SerializeFlags,
    VideoRate, escape, parse,
};

// ------------------------------------------------------------- strategies

fn arb_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..6).prop_map(|v| v.into_iter().collect())
}

fn arb_dict() -> impl Strategy<Value = Dict> {
    proptest::collection::vec((arb_text(), arb_text()), 0..4).prop_map(|pairs| {
        let mut d = Dict::new();
        let mut seen: Vec<String> = Vec::new();
        for (k, v) in pairs {
            // Duplicate keys collapse on parse, so only distinct keys can
            // survive a round trip. That is a property of the grammar, not a
            // defect: `a=1:a=2` means `a=2`.
            if seen.contains(&k) {
                continue;
            }
            seen.push(k.clone());
            d.set(&k, &v);
        }
        d
    })
}

fn arb_string_array() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(arb_text(), 0..4).prop_filter(
        "a one-element array holding the empty string is indistinguishable from an empty array",
        |v| !(v.len() == 1 && v[0].is_empty()),
    )
}

fn arb_pixfmt() -> impl Strategy<Value = TestPixFmt> {
    prop_oneof![
        Just(TestPixFmt::None),
        Just(TestPixFmt::Yuv420p),
        Just(TestPixFmt::Rgb24)
    ]
}

fn arb_samplefmt() -> impl Strategy<Value = TestSampleFmt> {
    prop_oneof![
        Just(TestSampleFmt::None),
        Just(TestSampleFmt::S16),
        Just(TestSampleFmt::Fltp)
    ]
}

fn arb_chlayout() -> impl Strategy<Value = TestChLayout> {
    prop_oneof![
        Just(TestChLayout::Unspec),
        Just(TestChLayout::Mono),
        Just(TestChLayout::Stereo)
    ]
}

fn arb_method() -> impl Strategy<Value = TMethod> {
    prop_oneof![
        Just(TMethod::None),
        Just(TMethod::Rectangular),
        Just(TMethod::Triangular),
        Just(TMethod::Shibata)
    ]
}

fn arb_all_kinds() -> impl Strategy<Value = AllKinds> {
    let numbers = (
        any::<u64>(),
        -1000_i32..=1000_i32,
        any::<i64>(),
        0_u32..=100_u32,
        any::<u64>(),
        -1.0_f64..=1.0_f64,
        any::<f32>().prop_filter("NaN is not equal to itself", |f| !f.is_nan()),
        any::<bool>(),
        any::<Option<bool>>(),
        any::<Option<i64>>(),
    );
    let values = (
        arb_text(),
        (any::<i32>(), any::<i32>()),
        proptest::collection::vec(any::<u8>(), 0..6),
        arb_dict(),
        (any::<u32>(), any::<u32>()),
        (any::<i32>(), any::<i32>()),
        any::<i64>(),
        (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>()),
    );
    let enums = (
        arb_pixfmt(),
        arb_samplefmt(),
        arb_chlayout(),
        arb_method(),
        proptest::collection::vec(any::<i32>(), 0..=8),
        arb_string_array(),
        0.0_f64..=10.0_f64,
        arb_text(),
    );

    (numbers, values, enums).prop_map(
        |(
            (flags, i, i64v, u, u64v, d, f, b, tri, opt_i),
            (s, (rn, rd), bin, dict, size, (vn, vd), dur, (cr, cg, cb, ca)),
            (pixfmt, samplefmt, chlayout, method, arr, sarr, gain, label),
        )| AllKinds {
            flags: TFlags::from_bits(flags),
            i,
            i64v,
            u,
            u64v,
            d,
            f,
            b,
            tri,
            s,
            r: Rational::new(rn, rd),
            bin: Binary(bin),
            dict,
            size,
            pixfmt,
            samplefmt,
            chlayout,
            rate: VideoRate(Rational::new(vn, vd)),
            dur: Duration(dur),
            colour: Rgba::new(cr, cg, cb, ca),
            method,
            arr,
            sarr,
            opt_i,
            child: ChildOpts { gain, label },
            cache: None,
        },
    )
}

// ------------------------------------------------------------- the property

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// The crate's central property, with `skip_defaults` off.
    #[test]
    fn serialise_then_parse_is_identity(x in arb_all_kinds()) {
        let text = x.serialize(SerializeFlags::default());
        let mut y = AllKinds::default();
        y.set_from_string(&text, "=", ":")
            .map_err(|e| TestCaseError::fail(format!("{e} while parsing {text:?}")))?;
        prop_assert_eq!(&y, &x, "text was {:?}", text);
    }

    /// The same, with `skip_defaults` on: omitted options must already hold
    /// their defaults in the fresh target.
    #[test]
    fn serialise_then_parse_is_identity_skipping_defaults(x in arb_all_kinds()) {
        let f = SerializeFlags { skip_defaults: true, ..SerializeFlags::default() };
        let text = x.serialize(f);
        let mut y = AllKinds::default();
        y.set_from_string(&text, "=", ":")
            .map_err(|e| TestCaseError::fail(format!("{e} while parsing {text:?}")))?;
        prop_assert_eq!(&y, &x, "text was {:?}", text);
    }

    /// Serialising is idempotent: the second pass produces the same text.
    #[test]
    fn serialisation_is_stable(x in arb_all_kinds()) {
        let a = x.serialize(SerializeFlags::default());
        let mut y = AllKinds::default();
        y.set_from_string(&a, "=", ":").unwrap();
        prop_assert_eq!(y.serialize(SerializeFlags::default()), a);
    }

    /// Anything `set_str` accepts satisfies the typed range check, and anything
    /// it rejects leaves the object byte-for-byte unchanged.
    #[test]
    fn range_invariance(start in arb_all_kinds(), name in "(i|u|d|i64|arr|child_gain)", v in "[-+]?[0-9a-zA-Z.|]{0,12}") {
        let mut o = start.clone();
        let before = o.clone();
        if o.set_str(&name, &v).is_ok() {
            let id = o.schema().find_recursive(&name).unwrap().1.id;
            // The check runs against whichever object owns the option.
            if name == "child_gain" {
                prop_assert!(o.child.check_range(id).is_ok());
            } else {
                prop_assert!(o.check_range(id).is_ok());
            }
        } else {
            prop_assert_eq!(&o, &before, "a rejected value mutated the object");
        }
    }

    /// `apply_dict` consumes exactly the keys `find_recursive` resolves and
    /// hands back the rest untouched.
    #[test]
    fn dict_application_partitions_by_resolvability(
        keys in proptest::collection::vec("(i|u|b|nope|other|child_gain)", 0..6)
    ) {
        let mut d = Dict::new();
        for k in &keys {
            // Distinct keys only; a dict cannot hold the same key twice here.
            // "1" is a valid value for every option this strategy names.
            if d.get(k).is_none() {
                d.set(k, "1");
            }
        }
        let mut o = AllKinds::default();
        let left = o.apply_dict(&d).unwrap();
        let schema = o.schema();
        for (k, v) in d.iter() {
            if schema.find_recursive(k).is_some() {
                prop_assert!(left.get(k).is_none(), "{} should have been consumed", k);
            } else {
                prop_assert_eq!(left.get(k), Some(v));
            }
        }
    }

    /// One level of escaping is invertible for every mode.
    #[test]
    fn escape_unescape_round_trips(s in arb_text(), special in "[:=|,]{0,3}") {
        for mode in [escape::Mode::Auto, escape::Mode::Backslash, escape::Mode::Quote] {
            let e = escape::escape(&s, &special, mode);
            prop_assert_eq!(escape::unescape(&e).unwrap(), s.clone(), "mode {:?}", mode);
        }
    }

    /// Splitting an escaped join recovers the parts.
    #[test]
    fn split_of_join_recovers_the_parts(parts in proptest::collection::vec(arb_text(), 1..5)) {
        let joined = parts
            .iter()
            .map(|p| escape::escape(p, ":", escape::Mode::Auto))
            .collect::<Vec<_>>()
            .join(":");
        let back: Vec<String> = escape::split(&joined, ':')
            .unwrap()
            .into_iter()
            .map(std::borrow::Cow::into_owned)
            .collect();
        prop_assert_eq!(back, parts);
    }

    /// The `+a-b` grammar accumulates and removes exactly the named bits.
    #[test]
    fn flag_grammar_round_trips(bits in any::<u64>()) {
        let consts = <TFlags as vaco_opts::OptEnumConsts>::CONSTS;
        let sctx = SerCtx { name: "f", consts, unit: Some("tflags"), array: None };
        let pctx = ParseCtx { name: "f", consts, unit: Some("tflags"), range: None, array: None };
        let mut text = String::new();
        vaco_opts::serialize_flag_bits(bits, &mut text, &sctx);
        prop_assert_eq!(vaco_opts::parse_flag_bits(0, &text, &pctx).unwrap(), bits);
    }

    /// Duration is parsed and rendered without going through `f64`, so the
    /// whole `i64` microsecond range round-trips exactly.
    #[test]
    fn duration_round_trips_exactly(us in any::<i64>()) {
        let text = parse::format_duration(Duration(us));
        prop_assert_eq!(parse::duration(&text).unwrap(), Duration(us));
    }

    #[test]
    fn colour_round_trips(r in any::<u8>(), g in any::<u8>(), b in any::<u8>(), a in any::<u8>()) {
        let c = Rgba::new(r, g, b, a);
        prop_assert_eq!(parse::color(&parse::format_color(c)).unwrap(), c);
    }

    #[test]
    fn image_size_round_trips(w in any::<u32>(), h in any::<u32>()) {
        let mut text = String::new();
        (w, h).serialize(&mut text, &SerCtx::bare("size"));
        prop_assert_eq!(parse::image_size(&text).unwrap(), (w, h));
    }

    #[test]
    fn dict_round_trips(d in arb_dict()) {
        let text = d.to_string_with('=', ':');
        let mut back = Dict::new();
        back.parse_string(&text, "=", ":", vaco_opts::DictFlags::exact()).unwrap();
        prop_assert_eq!(back, d);
    }

    /// Parsing never panics, whatever it is fed. This is the property a fuzz
    /// target would assert; it is here too so it runs on every `cargo test`.
    #[test]
    fn arbitrary_input_never_panics(s in ".{0,64}") {
        let mut o = AllKinds::default();
        let _ = o.set_from_string(&s, "=", ":");
        for name in ["i", "flags", "dur", "colour", "size", "r", "rate", "bin", "dict", "arr"] {
            let _ = o.set_str(name, &s);
        }
    }
}

#[test]
fn a_rejected_array_element_leaves_the_whole_array_untouched() {
    let mut o = AllKinds::default();
    o.set_str("arr", "1|2|3").unwrap();
    let err = o.set_str("arr", "1|nope|3").unwrap_err();
    assert!(matches!(err, OptError::InvalidValue { .. }));
    assert_eq!(o.arr, vec![1, 2, 3]);
}
