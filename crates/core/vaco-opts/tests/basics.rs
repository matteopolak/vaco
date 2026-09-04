//! Unit coverage: every base parsed valid, at the boundary, invalid and empty;
//! the flag grammar; const lookup; introspection; help; children.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::float_cmp
)]

mod support;

use support::{AllKinds, ChildOpts, TFlags, TMethod, TestChLayout, TestPixFmt, TestSampleFmt};
use vaco_core::{Duration, Rational};
use vaco_opts::{
    Binary, Dict, OptBase, OptError, OptFlags, OptId, OptValue, Options, OptionsExt, ParseCtx,
    Rgba, SerCtx, SerializeFlags, VideoRate, escape, help_entries, parse, schema_of,
};

fn subject() -> AllKinds {
    AllKinds::default()
}

// ------------------------------------------------------------ per-base parse

/// Parse `s` into a fresh `T` with no schema around it.
fn parse_one<T: OptValue + Default>(s: &str) -> Result<T, OptError> {
    let mut v = T::default();
    v.parse_into(s, &ParseCtx::bare("x"))?;
    Ok(v)
}

fn ser_one<T: OptValue>(v: &T) -> String {
    let mut out = String::new();
    v.serialize(&mut out, &SerCtx::bare("x"));
    out
}

#[test]
fn int_bases() {
    assert_eq!(parse_one::<i32>("42").unwrap(), 42);
    assert_eq!(parse_one::<i32>("-42").unwrap(), -42);
    assert_eq!(parse_one::<i32>("0x2a").unwrap(), 42);
    assert_eq!(parse_one::<i32>(&i32::MAX.to_string()).unwrap(), i32::MAX);
    assert_eq!(parse_one::<i32>(&i32::MIN.to_string()).unwrap(), i32::MIN);
    // one past the boundary
    assert!(parse_one::<i32>("2147483648").is_err());
    assert!(parse_one::<i32>("").is_err());
    assert!(parse_one::<i32>("nope").is_err());

    assert_eq!(parse_one::<i64>(&i64::MIN.to_string()).unwrap(), i64::MIN);
    assert_eq!(parse_one::<u32>("4294967295").unwrap(), u32::MAX);
    assert!(parse_one::<u32>("-1").is_err());
    assert_eq!(parse_one::<u64>("18446744073709551615").unwrap(), u64::MAX);
}

#[test]
fn float_bases() {
    assert_eq!(parse_one::<f64>("0.5").unwrap(), 0.5);
    assert_eq!(parse_one::<f64>("-1e3").unwrap(), -1000.0);
    assert!(parse_one::<f64>("inf").unwrap().is_infinite());
    assert!(parse_one::<f64>("").is_err());
    assert_eq!(parse_one::<f32>("0.25").unwrap(), 0.25_f32);
    assert_eq!(ser_one(&0.25_f32), "0.25");
}

#[test]
fn bool_base() {
    for s in ["1", "true", "on", "yes", "enable", "enabled"] {
        assert!(parse_one::<bool>(s).unwrap(), "{s}");
    }
    for s in ["0", "false", "off", "no", "disable", "disabled"] {
        assert!(!parse_one::<bool>(s).unwrap(), "{s}");
    }
    assert!(parse_one::<bool>("auto").is_err());
    assert!(parse_one::<bool>("").is_err());
    // Tri-state lives in the type, not in a -1 convention.
    assert_eq!(parse_one::<Option<bool>>("auto").unwrap(), None);
    assert_eq!(parse_one::<Option<bool>>("true").unwrap(), Some(true));
    assert_eq!(ser_one(&Option::<bool>::None), "auto");
}

#[test]
fn string_and_binary_and_dict() {
    assert_eq!(parse_one::<String>("hello").unwrap(), "hello");
    assert_eq!(parse_one::<String>("").unwrap(), "");

    assert_eq!(parse_one::<Binary>("00ff10").unwrap().0, vec![0, 255, 16]);
    assert_eq!(parse_one::<Binary>("00FF10").unwrap().0, vec![0, 255, 16]);
    assert!(parse_one::<Binary>("0").is_err(), "odd digit count");
    assert!(parse_one::<Binary>("zz").is_err());
    assert_eq!(ser_one(&Binary(vec![0, 255, 16])), "00ff10");

    let d = parse_one::<Dict>("a=1:b=2").unwrap();
    assert_eq!(d.get("a"), Some("1"));
    assert_eq!(d.get("b"), Some("2"));
    assert_eq!(parse_one::<Dict>("").unwrap().len(), 0);
    // An escaped separator stays inside the value.
    let d = parse_one::<Dict>(r"a=1\:2").unwrap();
    assert_eq!(d.get("a"), Some("1:2"));
}

#[test]
fn image_size_base() {
    assert_eq!(parse_one::<(u32, u32)>("1920x1080").unwrap(), (1920, 1080));
    assert_eq!(parse_one::<(u32, u32)>("hd1080").unwrap(), (1920, 1080));
    assert_eq!(parse_one::<(u32, u32)>("qcif").unwrap(), (176, 144));
    assert!(parse_one::<(u32, u32)>("1920").is_err());
    assert!(parse_one::<(u32, u32)>("").is_err());
    assert_eq!(ser_one(&(1920u32, 1080u32)), "1920x1080");
}

#[test]
fn rational_and_video_rate() {
    assert_eq!(
        parse_one::<Rational>("30000/1001").unwrap(),
        Rational::new(30000, 1001)
    );
    assert_eq!(parse_one::<Rational>("25").unwrap(), Rational::new(25, 1));
    assert!(parse_one::<Rational>("").is_err());
    assert_eq!(
        parse_one::<VideoRate>("ntsc").unwrap().0,
        Rational::new(30000, 1001)
    );
    assert_eq!(
        parse_one::<VideoRate>("pal").unwrap().0,
        Rational::new(25, 1)
    );
    assert_eq!(
        parse_one::<VideoRate>("ntsc-film").unwrap().0,
        Rational::new(24000, 1001)
    );
    // An undefined rate is 0/0, not zero.
    assert_eq!(ser_one(&VideoRate::default()), "0/0");
}

#[test]
fn duration_base() {
    assert_eq!(
        parse::duration("1").unwrap(),
        Duration::from_micros(1_000_000)
    );
    assert_eq!(
        parse::duration("1.5").unwrap(),
        Duration::from_micros(1_500_000)
    );
    assert_eq!(
        parse::duration("-1.5").unwrap(),
        Duration::from_micros(-1_500_000)
    );
    assert_eq!(
        parse::duration("5ms").unwrap(),
        Duration::from_micros(5_000)
    );
    assert_eq!(parse::duration("5us").unwrap(), Duration::from_micros(5));
    assert_eq!(
        parse::duration("2s").unwrap(),
        Duration::from_micros(2_000_000)
    );
    assert_eq!(
        parse::duration("1:02").unwrap(),
        Duration::from_micros(62_000_000)
    );
    assert_eq!(
        parse::duration("12:34:56").unwrap(),
        Duration::from_micros(45_296_000_000)
    );
    assert_eq!(
        parse::duration("-1:02.5").unwrap(),
        Duration::from_micros(-62_500_000)
    );
    assert!(parse::duration("").is_none());
    assert!(parse::duration("abc").is_none());
    assert!(parse::duration("1:2:3:4").is_none());
    // The fraction is parsed exactly, not through f64.
    assert_eq!(
        parse::duration("9223372036854.775807").unwrap(),
        Duration::from_micros(i64::MAX)
    );
}

#[test]
fn color_base() {
    assert_eq!(
        parse_one::<Rgba>("#ff0000").unwrap(),
        Rgba::new(255, 0, 0, 255)
    );
    assert_eq!(
        parse_one::<Rgba>("0xff000080").unwrap(),
        Rgba::new(255, 0, 0, 128)
    );
    assert_eq!(parse_one::<Rgba>("red").unwrap(), Rgba::new(255, 0, 0, 255));
    assert_eq!(parse_one::<Rgba>("Red").unwrap(), Rgba::new(255, 0, 0, 255));
    assert_eq!(parse_one::<Rgba>("red@0.5").unwrap().a, 128);
    assert!(parse_one::<Rgba>("").is_err());
    assert!(parse_one::<Rgba>("#gg0000").is_err());
    assert_eq!(ser_one(&Rgba::new(1, 2, 3, 4)), "0x01020304");
}

#[test]
fn layer_one_bases_are_contributed_from_outside() {
    // The whole point of F6: these impls live in the test crate, not here.
    assert_eq!(
        parse_one::<TestPixFmt>("yuv420p").unwrap(),
        TestPixFmt::Yuv420p
    );
    assert_eq!(
        parse_one::<TestSampleFmt>("s16").unwrap(),
        TestSampleFmt::S16
    );
    assert_eq!(
        parse_one::<TestChLayout>("stereo").unwrap(),
        TestChLayout::Stereo
    );
    let s = schema_of::<AllKinds>();
    assert_eq!(s.find("pixfmt").unwrap().kind.base, OptBase::PixelFmt);
    assert_eq!(s.find("samplefmt").unwrap().kind.base, OptBase::SampleFmt);
    assert_eq!(s.find("chlayout").unwrap().kind.base, OptBase::ChLayout);
}

// ------------------------------------------------------------------- units

#[test]
fn flag_accumulate_and_remove() {
    let mut o = subject();
    o.set_str("flags", "low_delay+bitexact").unwrap();
    assert_eq!(o.flags.bits(), 0b011);
    o.set_str("flags", "+unaligned").unwrap();
    assert_eq!(o.flags.bits(), 0b111);
    o.set_str("flags", "-bitexact").unwrap();
    assert_eq!(o.flags.bits(), 0b101);
    // No leading sign means absolute assignment.
    o.set_str("flags", "bitexact").unwrap();
    assert_eq!(o.flags.bits(), 0b010);
    // Raw integers mix with names.
    o.set_str("flags", "+0x4").unwrap();
    assert_eq!(o.flags.bits(), 0b110);
    o.set_str("flags", "0").unwrap();
    assert_eq!(o.flags.bits(), 0);
    assert!(matches!(
        o.set_str("flags", "+nope"),
        Err(OptError::UnknownConst { .. })
    ));
}

#[test]
fn flag_serialisation_round_trips_uncovered_bits() {
    let mut o = subject();
    o.flags = TFlags::from_bits(0b1_0000_0001);
    assert_eq!(o.get_str("flags").unwrap(), "low_delay+0x100");
    let mut p = subject();
    p.set_str("flags", &o.get_str("flags").unwrap()).unwrap();
    assert_eq!(p.flags, o.flags);
}

#[test]
fn named_constants_belong_to_the_unit() {
    let s = schema_of::<AllKinds>();
    let names: Vec<&str> = s.consts_for_unit("tmethod").map(|c| c.name).collect();
    assert_eq!(names, ["none", "rectangular", "triangular", "shibata"]);
    let flags: Vec<&str> = s.consts_for_unit("tflags").map(|c| c.name).collect();
    assert_eq!(flags, ["low_delay", "bitexact", "unaligned"]);
    assert_eq!(s.consts_for_unit("nosuchunit").count(), 0);
}

#[test]
fn enum_const_lookup_is_case_sensitive() {
    let mut o = subject();
    o.set_str("method", "shibata").unwrap();
    assert_eq!(o.method, TMethod::Shibata);
    assert_eq!(o.get_str("method").unwrap(), "shibata");
    // The explicit discriminant survives.
    o.set_str("method", "17").unwrap();
    assert_eq!(o.method, TMethod::Shibata);
    assert!(o.set_str("method", "Shibata").is_err());
    assert!(o.set_str("method", "99").is_err());
}

// ------------------------------------------------------------ aliases, names

#[test]
fn aliases_resolve() {
    let mut o = subject();
    o.set_str("tflags", "bitexact").unwrap();
    assert_eq!(o.flags, TFlags::BITEXACT);
    assert_eq!(o.get_str("tflags").unwrap(), o.get_str("flags").unwrap());
    assert!(matches!(
        o.set_str("no_such_option", "1"),
        Err(OptError::NotFound { .. })
    ));
}

// ----------------------------------------------------------------- ranges

#[test]
fn typed_range_is_enforced_and_rolls_back() {
    let mut o = subject();
    o.set_str("i", "1000").unwrap();
    assert_eq!(o.i, 1000);
    let err = o.set_str("i", "1001").unwrap_err();
    assert!(matches!(err, OptError::OutOfRange { .. }));
    assert_eq!(
        o.i, 1000,
        "a rejected value must leave the object unmodified"
    );
    assert!(o.set_str("i", "-1001").is_err());
    assert_eq!(o.i, 1000);
    // Out-of-type values are rejected before the range check.
    assert!(matches!(
        o.set_str("i", "nope"),
        Err(OptError::InvalidValue { .. })
    ));
    assert_eq!(o.i, 1000);
}

#[test]
fn int64_range_is_exact_above_two_to_the_53() {
    // The display pair is f64 and cannot represent this; the typed check can.
    let mut o = subject();
    o.set_str("i64", &i64::MAX.to_string()).unwrap();
    assert_eq!(o.i64v, i64::MAX);
    let r = o.query_ranges("i64").unwrap();
    assert_eq!(r.len(), 1);
    assert!(r[0].max >= 9.2e18);
}

#[test]
fn ranges_apply_elementwise_to_arrays() {
    let mut o = subject();
    o.set_str("arr", "1|2|3").unwrap();
    assert_eq!(o.arr, vec![1, 2, 3]);
    assert!(matches!(
        o.set_str("arr", "1|2|3|4|5|6|7|8|9"),
        Err(OptError::ArrayLen { .. })
    ));
    assert_eq!(o.arr, vec![1, 2, 3]);
}

// ---------------------------------------------------------------- k=v:k=v

#[test]
fn positional_arguments_follow_declaration_order() {
    let mut o = subject();
    o.set_from_string("bitexact:7", "=", ":").unwrap();
    assert_eq!(o.flags, TFlags::BITEXACT);
    assert_eq!(o.i, 7);
}

#[test]
fn positional_after_named_is_rejected() {
    let mut o = subject();
    let e = o.set_from_string("i=1:2", "=", ":").unwrap_err();
    assert!(matches!(e, OptError::PositionalAfterNamed { .. }));
}

#[test]
fn escaped_separators_survive_a_round_trip() {
    let mut o = subject();
    o.set_from_string(r"s=a\:b\=c", "=", ":").unwrap();
    assert_eq!(o.s, "a:b=c");
    let text = o.serialize(SerializeFlags {
        skip_defaults: true,
        ..SerializeFlags::default()
    });
    let mut p = subject();
    p.set_from_string(&text, "=", ":").unwrap();
    assert_eq!(p.s, "a:b=c");
}

#[test]
fn quoted_values_are_honoured() {
    let mut o = subject();
    o.set_from_string("s='a:b'", "=", ":").unwrap();
    assert_eq!(o.s, "a:b");
    o.set_from_string(r"s='a'\''b'", "=", ":").unwrap();
    assert_eq!(o.s, "a'b");
}

// -------------------------------------------------------------- children

#[test]
fn child_options_resolve_by_name() {
    let mut o = subject();
    o.set_str("child_gain", "2.5").unwrap();
    assert_eq!(o.child.gain, 2.5);
    assert_eq!(o.get_str("child_gain").unwrap(), "2.5");
    // The child's range is enforced through the parent.
    assert!(o.set_str("child_gain", "11").is_err());
    assert_eq!(o.child.gain, 2.5);

    let s = schema_of::<AllKinds>();
    assert_eq!(s.children.len(), 1);
    assert!(
        s.find("child_gain").is_none(),
        "not in the parent's own table"
    );
    assert!(s.find_recursive("child_gain").is_some());
}

// --------------------------------------------------------------- dict apply

#[test]
fn apply_dict_returns_the_unconsumed_keys() {
    let mut o = subject();
    let mut d = Dict::new();
    d.set("i", "5");
    d.set("child_gain", "3");
    d.set("not_an_option", "x");
    let left = o.apply_dict(&d).unwrap();
    assert_eq!(o.i, 5);
    assert_eq!(o.child.gain, 3.0);
    assert_eq!(left.len(), 1);
    assert_eq!(left.get("not_an_option"), Some("x"));
}

// ------------------------------------------------------------- defaults etc.

#[test]
fn defaults_and_is_set_to_default() {
    let mut o = subject();
    assert_eq!(o.d, 0.5);
    assert_eq!(o.f, 0.25_f32);
    assert_eq!(o.child.gain, 1.0);
    assert!(o.is_set_to_default("i").unwrap());
    o.set_str("i", "3").unwrap();
    assert!(!o.is_set_to_default("i").unwrap());
    assert!(o.is_set_to_default("child_gain").unwrap());
    o.set_str("child_gain", "9").unwrap();
    assert!(!o.is_set_to_default("child_gain").unwrap());
    assert_eq!(o.default_repr("d").unwrap(), "0.5");
    assert_eq!(o.default_repr("method").unwrap(), "none");
}

#[test]
fn skip_defaults_omits_untouched_options() {
    let mut o = subject();
    let f = SerializeFlags {
        skip_defaults: true,
        ..SerializeFlags::default()
    };
    assert_eq!(o.serialize(f), "");
    o.set_str("i", "3").unwrap();
    assert_eq!(o.serialize(f), "i=3");
    o.set_str("child_gain", "2").unwrap();
    assert_eq!(o.serialize(f), "i=3:child_gain=2");
}

#[test]
fn serialize_only_filters_by_flag() {
    let mut o = subject();
    o.set_str("i", "3").unwrap();
    o.set_str("samplefmt", "s16").unwrap();
    let f = SerializeFlags {
        skip_defaults: true,
        only: OptFlags::AUDIO,
        ..SerializeFlags::default()
    };
    assert_eq!(o.serialize(f), "samplefmt=s16");
}

// ------------------------------------------------------------- typed access

#[test]
fn typed_get_and_set() {
    let mut o = subject();
    o.set_typed("i", 12_i32).unwrap();
    assert_eq!(o.get_typed::<i32>("i").unwrap(), 12);
    assert!(matches!(
        o.get_typed::<i64>("i"),
        Err(OptError::TypeMismatch { .. })
    ));
    assert!(matches!(
        o.set_typed("i", 12_i64),
        Err(OptError::TypeMismatch { .. })
    ));
    // The typed path is still range checked.
    assert!(o.set_typed("i", 5000_i32).is_err());
    assert_eq!(o.i, 12);
}

// --------------------------------------------------------- runtime gating

#[test]
fn process_command_requires_the_runtime_flag() {
    let mut o = subject();
    o.process_command("i", "4").unwrap();
    assert_eq!(o.i, 4);
    assert!(matches!(
        o.process_command("u", "4"),
        Err(OptError::NotRuntime { .. })
    ));
    assert_eq!(o.u, 0);
    o.process_command("child_gain", "2").unwrap();
    assert_eq!(o.child.gain, 2.0);
}

// ------------------------------------------------------------ introspection

#[test]
fn schema_is_reachable_without_an_instance() {
    let s = schema_of::<AllKinds>();
    assert_eq!(s.class_name, "AllKinds");
    assert_eq!(s.options.len(), 24);
    // Declaration order is stable; positional arguments depend on it.
    let names: Vec<&str> = s.iter().map(|o| o.name).collect();
    assert_eq!(names[0], "flags");
    assert_eq!(names[1], "i");
    assert_eq!(*names.last().unwrap(), "opt_i");
    // Ids match indices.
    for (i, o) in s.iter().enumerate() {
        assert_eq!(o.id, OptId(u16::try_from(i).unwrap()));
    }
    assert_eq!(s.find_by_id(OptId(1)).unwrap().name, "i");
    assert_eq!(s.iter_recursive().count(), 26);
}

#[test]
fn every_base_except_const_appears_in_the_reference_object() {
    let s = schema_of::<AllKinds>();
    let mut missing = Vec::new();
    for b in OptBase::ALL {
        if b == OptBase::Const {
            continue;
        }
        if !s.iter().any(|o| o.kind.base == b) {
            missing.push(b);
        }
    }
    assert!(
        missing.is_empty(),
        "bases not covered by the test object: {missing:?}"
    );
}

#[test]
fn array_kinds_carry_their_modifier() {
    let s = schema_of::<AllKinds>();
    let arr = s.find("arr").unwrap();
    assert_eq!(arr.kind.base, OptBase::Int);
    let a = arr.kind.array.unwrap();
    assert_eq!(a.sep, '|');
    assert_eq!(a.max_len, 8);
    assert_eq!(arr.kind.type_name(), "[int]");
    assert_eq!(s.find("sarr").unwrap().kind.array.unwrap().sep, ',');
    assert!(s.find("i").unwrap().kind.array.is_none());
}

// ------------------------------------------------------------------- help

#[test]
fn help_entries_carry_the_facts_the_cli_prints() {
    let s = schema_of::<AllKinds>();
    let all = help_entries(s, OptFlags::empty());
    assert_eq!(all.len(), s.options.len());
    let audio = help_entries(s, OptFlags::AUDIO);
    let names: Vec<&str> = audio.iter().map(|e| e.name).collect();
    assert_eq!(names, ["samplefmt", "chlayout"]);

    let e = all.iter().find(|e| e.name == "i").unwrap();
    assert_eq!(e.kind.type_name(), "int");
    assert_eq!(e.help, "an int");
    assert_eq!(e.default_repr, "0");
    assert_eq!(e.range.unwrap().min, -1000.0);
    assert_eq!(
        String::from_utf8(e.flags_column.to_vec()).unwrap(),
        "...V.....T."
    );

    let m = all.iter().find(|e| e.name == "method").unwrap();
    assert_eq!(m.consts.len(), 4);
    assert_eq!(m.consts[3].name, "shibata");
    assert_eq!(m.consts[3].help, "Shibata noise shaping");
}

#[test]
fn flag_column_layout() {
    assert_eq!(OptFlags::empty().column_string(), "...........");
    assert_eq!(
        OptFlags::FILTERING
            .union(OptFlags::VIDEO)
            .union(OptFlags::RUNTIME)
            .column_string(),
        "..FV.....T."
    );
    assert_eq!(OptFlags::PARAM.column_string(), "ED.........");
    assert_eq!(OptFlags::DEPRECATED.column_string(), "..........P");
    assert!(OptFlags::PARAM.contains(OptFlags::ENCODING));
    assert!(!OptFlags::ENCODING.contains(OptFlags::PARAM));
    assert!(OptFlags::empty().contains(OptFlags::empty()));
    assert_eq!(OptFlags::from_attr_name("param"), Some(OptFlags::PARAM));
    assert_eq!(OptFlags::from_attr_name("nope"), None);
}

// ------------------------------------------------------------------ escape

#[test]
fn escaping_levels() {
    use escape::Mode;
    assert_eq!(escape::escape("plain", ":=", Mode::Auto), "plain");
    assert_eq!(escape::escape("a:b", ":=", Mode::Auto), r"a\:b");
    assert_eq!(escape::escape("a:b", ":=", Mode::Quote), "'a:b'");
    assert_eq!(escape::escape("a'b", ":=", Mode::Quote), "'a'\\''b'");
    assert_eq!(escape::unescape("'a'\\''b'").unwrap(), "a'b");
    // D17: both are accepted, not errors — the reference opens `movie='ab` as
    // the file `ab` and `movie=ab\` as `ab\`. See vaco-core's escape tests.
    assert_eq!(escape::unescape("'unterminated").unwrap(), "unterminated");
    assert_eq!(escape::unescape(r"trailing\").unwrap(), r"trailing\");

    let parts = escape::split(r"a\:b:c", ':').unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], "a:b");
    assert_eq!(parts[1], "c");
    // split_raw keeps the escapes for the next level down.
    assert_eq!(
        escape::split_raw(r"a\:b:c", ":").unwrap(),
        vec![r"a\:b", "c"]
    );
}

// ------------------------------------------------------------------ readonly

#[test]
fn defaults_object_is_a_real_instance() {
    let o = subject();
    let d = o.defaults();
    assert_eq!(d.schema().class_name, "AllKinds");
    assert!(d.slot(OptId(0)).is_some());
    assert!(d.slot(OptId(9999)).is_none());
}

#[test]
fn child_default_impl_is_generated() {
    let c = ChildOpts::default();
    assert_eq!(c.gain, 1.0);
    assert_eq!(c.label, "");
}
