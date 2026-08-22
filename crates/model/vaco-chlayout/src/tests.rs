//! Unit, golden and property tests.
//!
//! # The golden table is the acceptance criterion
//!
//! [`GOLDEN`] is a recorded transcript, not a set of expectations someone wrote
//! down: each row is a string fed to `FFmpeg` 8.1 as `-ch_layout`, paired with the
//! layout description the tool then printed in its stream banner — or `None`
//! where it refused the string. It was produced by a script, in one pass, and
//! pasted in unedited. See `docs/model/vaco-chlayout.md` for how to regenerate
//! it against a new reference build.
//!
//! Every row is a *parse* assertion and a *describe* assertion at once, because
//! the reference's description is itself a valid input: `from_name(s)` must
//! produce a layout whose `describe()` is exactly the recorded text.
#![expect(
    clippy::expect_used,
    reason = "a test that unwraps a None is a failing test, which is the \
              correct outcome; the lint exists to stop library code panicking \
              on hostile input"
)]

use proptest::prelude::*;

use super::{Channel, ChannelEntry, ChannelLayout, ChannelOrder, Label, table};

#[rustfmt::skip]
const GOLDEN: [(&str, Option<&str>); 234] = [
    ("mono", Some("mono")),
    ("stereo", Some("stereo")),
    ("2.1", Some("2.1")),
    ("3.0", Some("3.0")),
    ("3.0(back)", Some("3.0(back)")),
    ("4.0", Some("4.0")),
    ("quad", Some("quad")),
    ("quad(side)", Some("quad(side)")),
    ("3.1", Some("3.1")),
    ("5.0", Some("5.0")),
    ("5.0(side)", Some("5.0(side)")),
    ("4.1", Some("4.1")),
    ("5.1", Some("5.1")),
    ("5.1(side)", Some("5.1(side)")),
    ("6.0", Some("6.0")),
    ("6.0(front)", Some("6.0(front)")),
    ("3.1.2", Some("3.1.2")),
    ("hexagonal", Some("hexagonal")),
    ("6.1", Some("6.1")),
    ("6.1(back)", Some("6.1(back)")),
    ("6.1(front)", Some("6.1(front)")),
    ("7.0", Some("7.0")),
    ("7.0(front)", Some("7.0(front)")),
    ("7.1", Some("7.1")),
    ("7.1(wide)", Some("7.1(wide)")),
    ("7.1(wide-side)", Some("7.1(wide-side)")),
    ("5.1.2", Some("5.1.2")),
    ("5.1.2(back)", Some("5.1.2(back)")),
    ("octagonal", Some("octagonal")),
    ("cube", Some("cube")),
    ("5.1.4", Some("5.1.4")),
    ("7.1.2", Some("7.1.2")),
    ("7.1.4", Some("7.1.4")),
    ("7.2.3", Some("7.2.3")),
    ("9.1.4", Some("9.1.4")),
    ("9.1.6", Some("9.1.6")),
    ("hexadecagonal", Some("hexadecagonal")),
    ("binaural", Some("binaural")),
    ("downmix", Some("downmix")),
    ("22.2", Some("22.2")),
    ("FL+FR", Some("stereo")),
    ("FL+FC", Some("2 channels (FL+FC)")),
    ("FC+FL", Some("2 channels (FC+FL)")),
    ("FL+FL", Some("2 channels (FL+FL)")),
    ("TSL+LFE2", Some("2 channels (TSL+LFE2)")),
    ("LFE2+TSL", Some("2 channels (LFE2+TSL)")),
    ("FL+UNK", Some("2 channels (FL+UNK)")),
    ("UNK+FL", Some("2 channels (UNK+FL)")),
    ("UNK", Some("1 channels")),
    ("UNK+UNK", Some("2 channels")),
    ("UNSD", Some("1 channels (UNSD)")),
    ("FL+UNSD", Some("2 channels (FL+UNSD)")),
    ("FL+", Some("1 channels (FL)")),
    ("FL+FR+", Some("stereo")),
    ("FL++FR", None),
    ("+FL", None),
    ("", None),
    ("+", None),
    ("FL +FR", Some("stereo")),
    ("FL+ FR", Some("stereo")),
    (" FL", Some("1 channels (FL)")),
    ("FL ", Some("1 channels (FL)")),
    ("F L", None),
    ("4 channels", Some("4 channels")),
    (" 4 channels", Some("4 channels")),
    ("4 channels ", None),
    ("4  channels", None),
    ("1 channels", Some("1 channels")),
    ("0 channels", None),
    ("1C", Some("1 channels")),
    ("2C", Some("2 channels")),
    ("3C", Some("3 channels")),
    ("6C", Some("6 channels")),
    ("8C", Some("8 channels")),
    ("0C", None),
    ("06C", Some("6 channels")),
    ("+6C", Some("6 channels")),
    ("0x6C", Some("4 channels (FC+LFE+BR+FLC)")),
    ("1c", Some("mono")),
    ("2c", Some("stereo")),
    ("3c", Some("2.1")),
    ("4c", Some("4.0")),
    ("5c", Some("5.0")),
    ("6c", Some("5.1")),
    ("7c", Some("6.1")),
    ("8c", Some("7.1")),
    ("9c", None),
    ("10c", Some("5.1.4")),
    ("11c", None),
    ("12c", Some("7.1.4")),
    ("14c", Some("9.1.4")),
    ("16c", Some("9.1.6")),
    ("17c", None),
    ("24c", Some("22.2")),
    ("010c", Some("5.1.4")),
    ("08c", Some("7.1")),
    ("0x4c", Some("3 channels (FC+LFE+FLC)")),
    ("0", None),
    ("4", Some("mono")),
    ("63", Some("5.1")),
    ("+63", Some("5.1")),
    (" 63", Some("5.1")),
    ("63 ", None),
    ("010", Some("1 channels (LFE)")),
    ("0x3f", Some("5.1")),
    ("0X3F", Some("5.1")),
    ("0b111", None),
    ("-1", None),
    ("-0x3f", None),
    ("0x0", None),
    ("0x8000000000000000", Some("1 channels (USR63)")),
    ("0x4000000000000000", Some("1 channels (BIR)")),
    ("0x1f80003ffff", Some("22.2")),
    ("0x300002d6ff", Some("9.1.6")),
    ("0x6000000000000000", Some("binaural")),
    ("0x80001563f", Some("7.2.3")),
    ("ambisonic 1", Some("ambisonic 1")),
    ("ambisonic 2", Some("ambisonic 2")),
    ("ambisonic 0", Some("ambisonic 0")),
    ("ambisonic 3", Some("ambisonic 3")),
    ("ambisonic 1+stereo", Some("ambisonic 1+stereo")),
    ("ambisonic 1+FC", Some("ambisonic 1+mono")),
    ("ambisonic 1+FL+FC", Some("ambisonic 1+2 channels (FL+FC)")),
    ("ambisonic 1+FC+FL", Some("ambisonic 1+2 channels (FC+FL)")),
    ("ambisonic 0+mono", Some("ambisonic 0+mono")),
    ("ambisonic +stereo", Some("ambisonic 0+stereo")),
    ("ambisonic  1", Some("ambisonic 1")),
    ("ambisonic 1 ", None),
    ("ambisonic 1+", None),
    ("ambisonic", None),
    ("ambisonic -1", None),
    ("ambisonic 0x2", Some("ambisonic 2")),
    ("ambisonic 1+ambisonic 1", None),
    ("AMBISONIC", Some("ambisonic 0")),
    ("AMBI0", Some("ambisonic 0")),
    ("AMBI1", Some("1 channels (AMBI1)")),
    ("AMBI3", Some("1 channels (AMBI3)")),
    ("AMBI0+AMBI1+AMBI2+AMBI3", Some("ambisonic 1")),
    ("AMBI0+AMBI1+AMBI2", Some("3 channels (AMBI0+AMBI1+AMBI2)")),
    ("AMBI0+AMBI1+AMBI2+AMBI3+AMBI4", Some("5 channels (AMBI0+AMBI1+AMBI2+AMBI3+AMBI4)")),
    ("AMBI0+AMBI1+AMBI2+AMBI3+FC", Some("ambisonic 1+mono")),
    ("AMBI1+AMBI0+AMBI2+AMBI3", Some("4 channels (AMBI1+AMBI0+AMBI2+AMBI3)")),
    ("FC+AMBI0+AMBI1+AMBI2+AMBI3", Some("5 channels (FC+AMBI0+AMBI1+AMBI2+AMBI3)")),
    ("AMBI0+AMBI2", Some("2 channels (AMBI0+AMBI2)")),
    ("AMBI0+FC", Some("ambisonic 0+mono")),
    ("AMBI1023", Some("1 channels (AMBI1023)")),
    ("AMBI1024", None),
    ("AMBI-1", None),
    ("AMBI 1", Some("1 channels (AMBI1)")),
    ("AMBI0x2", Some("1 channels (AMBI2)")),
    ("USR0", Some("1 channels (FL)")),
    ("USR2", Some("mono")),
    ("USR18", Some("1 channels (USR18)")),
    ("USR29", Some("1 channels (DL)")),
    ("USR63", Some("1 channels (USR63)")),
    ("USR64", Some("1 channels (USR64)")),
    ("USR511", Some("1 channels (USR511)")),
    ("USR512", Some("1 channels (UNSD)")),
    ("USR513", Some("1 channels (USR513)")),
    ("USR768", Some("1 channels")),
    ("USR1024", Some("ambisonic 0")),
    ("USR2047", Some("1 channels (AMBI1023)")),
    ("USR2048", Some("1 channels (USR2048)")),
    ("USR018", None),
    ("USR010", Some("1 channels (BC)")),
    ("USR0x10", Some("1 channels (TBC)")),
    ("USR 18", Some("1 channels (USR18)")),
    ("USR-1", None),
    ("USR2147483647", Some("1 channels (USR2147483647)")),
    ("USR2147483648", None),
    ("USR18+USR19", Some("2 channels (USR18+USR19)")),
    ("FL@", Some("1 channels (FL)")),
    ("FL@x+FR", Some("2 channels (FL@x+FR)")),
    ("FL@+FR", Some("stereo")),
    ("FL@Left+FR@Right", Some("2 channels (FL@Left+FR@Right)")),
    ("FL@a@b", Some("1 channels (FL@a@b)")),
    ("FL@0123456789abcdef", Some("1 channels (FL@0123456789abcde)")),
    ("@x", None),
    ("NONE", None),
    ("UNKNOWN", None),
    ("x", None),
    ("MONO", None),
    ("mono ", None),
    (" 5.1", None),
    ("5.1 ", None),
    ("2 channels (FL+FC)", Some("2 channels (FL+FC)")),
    ("2 channels(FL+FC)", Some("2 channels (FL+FC)")),
    ("2  channels (FL+FC)", Some("2 channels (FL+FC)")),
    ("2channels (FL+FC)", Some("2 channels (FL+FC)")),
    (" 2 channels (FL+FC)", Some("2 channels (FL+FC)")),
    ("2 channels (FL+FR)", Some("stereo")),
    ("2 channels (FC+FL)", Some("2 channels (FC+FL)")),
    ("1 channels (UNSD)", Some("1 channels (UNSD)")),
    ("1 channels (FL)", Some("1 channels (FL)")),
    ("24 channels (FL+FR+FC+LFE+BL+BR+FLC+FRC+BC+SL+SR+TC+TFL+TFC+TFR+TBL+TBC+TBR+LFE2+TSL+TSR+BFC+BFL+BFR)", Some("22.2")),
    ("ambisonic 1+2 channels (FL+FC)", Some("ambisonic 1+2 channels (FL+FC)")),
    ("5 channels (FL+FC)", None),
    ("3 channels (FL+FC)", None),
    ("0 channels (FL+FC)", None),
    ("2 channels ()", None),
    ("2 channels (FL+FC", None),
    ("2 channels (FL+FC) ", None),
    ("2 channels (FL+FC))", None),
    ("2 channels ((FL+FC)", None),
    ("2 channels (FL+(FC)", None),
    ("(FL+FC)", None),
    ("2 xyz (FL+FC)", None),
    ("2 (FL+FC)", None),
    ("2(FL+FC)", None),
    ("0x2 channels (FL+FC)", None),
    ("2 CHANNELS (FL+FC)", None),
    ("2 channels (FL@x+FR)", Some("2 channels (FL@x+FR)")),
    ("FL@abcdefghijklmn", Some("1 channels (FL@abcdefghijklmn)")),
    ("FL@abcdefghijklmno", Some("1 channels (FL@abcdefghijklmno)")),
    ("FL@abcdefghijklmnop", Some("1 channels (FL@abcdefghijklmno)")),
    ("FL@abcdefghijklmnopq", Some("1 channels (FL@abcdefghijklmno)")),
    ("FL@x", Some("1 channels (FL@x)")),
    ("FC@x", Some("1 channels (FC@x)")),
    ("UNK@x", Some("1 channels (UNK@x)")),
    ("UNK@x+UNK", Some("2 channels (UNK@x+UNK)")),
    ("FL@x+FC", Some("2 channels (FL@x+FC)")),
    ("FL@x+FR@", Some("2 channels (FL@x+FR)")),
    ("2 channels (FL@+FR)", Some("stereo")),
    ("AMBI0@z", Some("ambisonic 0")),
    ("AMBI0@z+AMBI1+AMBI2+AMBI3", Some("ambisonic 1")),
    ("AMBI0@z+AMBI1+AMBI2+AMBI3+FC", Some("ambisonic 1+mono")),
    ("ambisonic 1+FL@x+FR", Some("ambisonic 1+2 channels (FL@x+FR)")),
    ("ambisonic 1+FL@x", Some("ambisonic 1+1 channels (FL@x)")),
    ("AMBI0+AMBI1+AMBI2+AMBI3+FL@x+FR", Some("ambisonic 1+2 channels (FL@x+FR)")),
    ("ambisonic 1+2 channels (FL@x+FR)", Some("ambisonic 1+2 channels (FL@x+FR)")),
    ("USR10@24", Some("1 channels (SR@24)")),
    ("USR@8", Some("1 channels (FL@8)")),
    ("AMBI4@65", Some("1 channels (AMBI4@65)")),
    ("2 channels (FC@a+FL@b)", Some("2 channels (FC@a+FL@b)")),
];

/// The one class we knowingly do not reproduce: a label truncated *mid
/// character*. Recorded in the same pass as [`GOLDEN`], and kept here so the gap
/// is visible rather than absent. See `Label`'s D17 note for why.
///
/// `(input, what the reference prints, what we print)`. The reference's column
/// is written with the broken byte spelled as an escape, because it is not
/// valid UTF-8 and so cannot appear literally in Rust source — which is most of
/// the reason we cannot reproduce it.
#[rustfmt::skip]
const LABEL_TRUNCATION_DIVERGENCE: [(&str, &[u8], &str); 3] = [
    // Nine `é` is 18 bytes; the reference keeps 15, cutting the eighth in half.
    ("FL@ééééééééé",       b"1 channels (FL@\xc3\xa9\xc3\xa9\xc3\xa9\xc3\xa9\xc3\xa9\xc3\xa9\xc3\xa9\xc3)",
                           "1 channels (FL@ééééééé)"),
    ("FL@aaéééééééé",      b"1 channels (FL@aa\xc3\xa9\xc3\xa9\xc3\xa9\xc3\xa9\xc3\xa9\xc3\xa9\xc3)",
                           "1 channels (FL@aaéééééé)"),
    ("FL@aaaaaaaaaaaaaaé", b"1 channels (FL@aaaaaaaaaaaaaa\xc3)",
                           "1 channels (FL@aaaaaaaaaaaaaa)"),
];

// ------------------------------------------------------------------- golden

#[test]
fn parses_exactly_what_the_reference_parses() {
    let mut wrong = Vec::new();
    for (input, expected) in GOLDEN {
        let got = ChannelLayout::from_name(input).map(|l| l.describe());
        let got = got.as_deref();
        if got != expected {
            wrong.push(format!("  {input:?}: expected {expected:?}, got {got:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} reference cases disagree:\n{}",
        wrong.len(),
        GOLDEN.len(),
        wrong.join("\n")
    );
}

#[test]
fn every_description_parses_back_to_itself() {
    // The reference's description is a valid input to its own parser, and the
    // second pass must be a fixed point. This is the property that keeps
    // `ffprobe` output round-trippable through `-ch_layout`.
    for (_, expected) in GOLDEN {
        let Some(text) = expected else { continue };
        let again = ChannelLayout::from_name(text)
            .expect("a description the reference emits must parse back");
        assert_eq!(again.describe(), text, "{text:?} is not a fixed point");
    }
}

#[test]
fn the_full_mask_names_every_bit() {
    // The one case the golden table cannot carry: the reference truncates a
    // description at the *caller's* buffer, and the two callers disagree — the
    // stream banner cuts at 228 bytes and `ffprobe` at 128. `describe` itself is
    // unbounded, so we assert the full string and check the recorded prefix.
    const BANNER_PREFIX: &str = "64 channels (FL+FR+FC+LFE+BL+BR+FLC+FRC+BC+SL+SR+TC+TFL+TFC+TFR+\
        TBL+TBC+TBR+USR18+USR19+USR20+USR21+USR22+USR23+USR24+USR25+USR26+USR27+USR28+DL+DR+WL+WR+\
        SDL+SDR+LFE2+TSL+TSR+BFC+BFL+BFR+SSL+SSR+TTL+TTR+USR45+USR46+USR47+USR48+";

    let all = ChannelLayout::from_name("0xffffffffffffffff").expect("full mask parses");
    assert_eq!(all.channels, 64);
    let text = all.describe();
    assert!(
        text.starts_with(BANNER_PREFIX),
        "diverges from the reference within its first 228 bytes:\n{text}"
    );
    assert!(text.ends_with("+BIL+BIR+USR63)"));
    // Every bit is present exactly once, in ascending order.
    for bit in 0..64u8 {
        assert_eq!(
            all.channel_at(u32::from(bit)),
            Channel::from_id(u32::from(bit))
        );
    }
}

// -------------------------------------------------------------- the tables

#[test]
fn named_masks_are_unique() {
    let mut masks: Vec<u64> = table::LAYOUTS.iter().map(|&(_, m)| m).collect();
    masks.sort_unstable();
    masks.dedup();
    assert_eq!(
        masks.len(),
        table::LAYOUTS.len(),
        "two layouts share a mask"
    );
}

#[test]
fn named_layout_names_are_unique() {
    let mut names: Vec<&str> = table::LAYOUTS.iter().map(|&(n, _)| n).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), table::LAYOUTS.len());
}

#[test]
fn every_standard_layout_round_trips() {
    for (name, layout) in ChannelLayout::standard() {
        assert_eq!(layout.name(), Some(name));
        assert_eq!(layout.describe(), name);
        assert_eq!(ChannelLayout::from_name(name).as_ref(), Some(&layout));
        assert_eq!(layout.channels, layout.mask().count_ones());
        assert!(layout.is_valid());
        // The mask form and the name form are the same layout.
        let via_mask = format!("{:#x}", layout.mask());
        assert_eq!(ChannelLayout::from_name(&via_mask).as_ref(), Some(&layout));
    }
}

#[test]
fn channel_table_is_consistent() {
    let mut bits: Vec<u8> = Vec::new();
    let mut names: Vec<&str> = Vec::new();
    for (channel, bit, short, description) in table::CHANNELS {
        assert_eq!(channel.id(), u32::from(bit), "{short} has the wrong id");
        assert_eq!(channel.bit(), Some(bit));
        assert_eq!(channel.short_name(), Some(short));
        assert_eq!(channel.description(), Some(description));
        assert_eq!(channel.to_string(), short);
        assert_eq!(Channel::from_name(short), Some(channel));
        assert_eq!(Channel::from_id(u32::from(bit)), Some(channel));
        bits.push(bit);
        names.push(short);
    }
    let sorted = {
        let mut b = bits.clone();
        b.sort_unstable();
        b
    };
    assert_eq!(bits, sorted, "the channel table is not in bit order");
    bits.dedup();
    assert_eq!(bits.len(), table::CHANNELS.len(), "duplicate bit");
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), table::CHANNELS.len(), "duplicate name");
}

#[test]
fn the_special_channels_have_the_ids_the_reference_gives_them() {
    // Recovered from `-ch_layout USR<n>`: 512 prints as `UNSD`, 768 as the
    // whole layout collapsing to unspecified, and 1024..=2047 as `AMBI<n>`.
    assert_eq!(Channel::Unused.id(), 512);
    assert_eq!(Channel::Unknown.id(), 768);
    assert_eq!(Channel::Ambisonic(0).id(), 1024);
    assert_eq!(Channel::Ambisonic(1023).id(), 2047);
    assert_eq!(Channel::from_id(512), Some(Channel::Unused));
    assert_eq!(Channel::from_id(768), Some(Channel::Unknown));
    assert_eq!(Channel::from_id(2047), Some(Channel::Ambisonic(1023)));
    assert_eq!(Channel::from_id(2048), Some(Channel::Unnamed(2048)));
    assert_eq!(Channel::from_id(i32::MAX as u32 + 1), None);
    // `UNSD` parses, `NONE` does not; `UNK` parses, `UNKNOWN` does not.
    assert_eq!(Channel::from_name("UNSD"), Some(Channel::Unused));
    assert_eq!(Channel::from_name("UNK"), Some(Channel::Unknown));
    assert_eq!(Channel::from_name("NONE"), None);
    assert_eq!(Channel::from_name("UNKNOWN"), None);
}

#[test]
fn channel_identity_is_the_id_not_the_variant() {
    assert_eq!(Channel::Unnamed(2), Channel::FrontCenter);
    assert!(Channel::FrontLeft < Channel::FrontRight);
    assert!(Channel::TopBackRight < Channel::DownmixLeft);
    assert!(Channel::BinauralRight < Channel::Unused);
    assert!(Channel::Unused < Channel::Unknown);
    assert!(Channel::Unknown < Channel::Ambisonic(0));
    // Sorting a channel list puts it in mask order.
    let mut v = vec![Channel::SideLeft, Channel::FrontLeft, Channel::LowFrequency];
    v.sort_unstable();
    assert_eq!(
        v,
        [Channel::FrontLeft, Channel::LowFrequency, Channel::SideLeft]
    );
}

#[test]
fn unnamed_bits_are_exactly_the_gaps() {
    let gaps: Vec<u32> = (0..64)
        .filter(|&b| matches!(Channel::from_id(b), Some(Channel::Unnamed(_))))
        .collect();
    let expected: Vec<u32> = (18..=28)
        .chain(45..=60)
        .chain(std::iter::once(63))
        .collect();
    assert_eq!(gaps, expected);
}

// ------------------------------------------------------------- the defaults

#[test]
fn default_for_is_the_first_table_entry_with_that_count() {
    // Recorded from `-ch_layout <n>c`. The counts with no entry are errors, not
    // fallbacks — that is what makes `9c` and `18c` fail.
    for (n, expected) in [
        (1u32, Some("mono")),
        (2, Some("stereo")),
        (3, Some("2.1")),
        (4, Some("4.0")),
        (5, Some("5.0")),
        (6, Some("5.1")),
        (7, Some("6.1")),
        (8, Some("7.1")),
        (9, None),
        (10, Some("5.1.4")),
        (11, None),
        (12, Some("7.1.4")),
        (13, None),
        (14, Some("9.1.4")),
        (15, None),
        (16, Some("9.1.6")),
        (17, None),
        (18, None),
        (20, None),
        (22, None),
        (24, Some("22.2")),
        (0, None),
        (65, None),
    ] {
        assert_eq!(
            ChannelLayout::default_for(n).and_then(|l| l.name()),
            expected,
            "default for {n} channels"
        );
    }
}

// ------------------------------------------------------- structure and index

#[test]
fn native_layouts_index_in_mask_order() {
    let l = ChannelLayout::from_name("5.1").expect("5.1");
    let expected = [
        Channel::FrontLeft,
        Channel::FrontRight,
        Channel::FrontCenter,
        Channel::LowFrequency,
        Channel::BackLeft,
        Channel::BackRight,
    ];
    for (i, ch) in expected.into_iter().enumerate() {
        assert_eq!(l.channel_at(i as u32), Some(ch));
        assert_eq!(l.index_of(ch), Some(i as u32));
        assert!(l.contains(ch));
    }
    assert_eq!(l.channel_at(6), None);
    assert!(!l.contains(Channel::SideLeft));
    assert_eq!(l.index_of(Channel::SideLeft), None);
    assert_eq!(l.iter().collect::<Vec<_>>(), expected.to_vec());
}

#[test]
fn custom_layouts_index_as_written() {
    let l = ChannelLayout::from_name("FC+FL").expect("custom");
    assert!(matches!(l.order, ChannelOrder::Custom(_)));
    assert_eq!(l.channel_at(0), Some(Channel::FrontCenter));
    assert_eq!(l.channel_at(1), Some(Channel::FrontLeft));
    assert_eq!(l.index_of(Channel::FrontLeft), Some(1));
    assert_eq!(l.mask(), 0, "a custom layout has no mask");
    assert!(l.is_valid());
}

#[test]
fn unspecified_layouts_have_channels_but_no_positions() {
    let l = ChannelLayout::unspecified(4);
    assert_eq!(l.channels, 4);
    assert_eq!(l.channel_at(0), Some(Channel::Unknown));
    assert_eq!(l.channel_at(4), None);
    assert_eq!(l.index_of(Channel::Unknown), None);
    assert_eq!(l.name(), None);
    assert_eq!(l.mask(), 0);
    assert_eq!(l.describe(), "4 channels");
    assert!(l.is_valid());
}

#[test]
fn ambisonic_layouts_are_acn_then_extras() {
    let l = ChannelLayout::from_name("ambisonic 1+stereo").expect("ambisonic");
    assert_eq!(l.channels, 6);
    for i in 0..4u16 {
        assert_eq!(l.channel_at(u32::from(i)), Some(Channel::Ambisonic(i)));
    }
    assert_eq!(l.channel_at(4), Some(Channel::FrontLeft));
    assert_eq!(l.channel_at(5), Some(Channel::FrontRight));
    assert_eq!(l.channel_at(6), None);
    assert_eq!(l.index_of(Channel::FrontRight), Some(5));
    assert!(l.is_valid());

    // Order n has (n+1)^2 components.
    for order in 0u16..8 {
        let l = ChannelLayout::ambisonic(order, []).expect("valid order");
        assert_eq!(l.channels, (u32::from(order) + 1).pow(2));
        assert_eq!(l.describe(), format!("ambisonic {order}"));
    }
    // Extras may not themselves be ambisonic.
    assert_eq!(ChannelLayout::ambisonic(1, [Channel::Ambisonic(9)]), None);
}

#[test]
fn the_ambisonic_order_limit_is_where_the_reference_puts_it() {
    // The reference squares the order into an `int` and rejects the layout when
    // that overflows: 46339 -> 46340^2 = 2_147_395_600 parses (and fails later,
    // downstream, on the absurd channel count); 46340 -> 46341^2 does not parse
    // at all. `order` is a `u16` so that the whole accepted range is reachable
    // and our rejection lands exactly where theirs does.
    for order in [0u32, 1, 3, 255, 256, 1000, 46_339] {
        let text = format!("ambisonic {order}");
        let l = ChannelLayout::from_name(&text).expect("within the accepted range");
        assert_eq!(l.channels, (order + 1) * (order + 1));
        assert_eq!(l.describe(), text);
    }
    for order in [46_340u32, 65_535, 65_536, 100_000] {
        assert_eq!(
            ChannelLayout::from_name(&format!("ambisonic {order}")),
            None,
            "ambisonic {order} should be rejected at parse"
        );
    }
    // The same boundary through the constructor, not just the parser.
    assert!(ChannelLayout::ambisonic(46_339, []).is_some());
    assert_eq!(ChannelLayout::ambisonic(46_340, []), None);
    assert_eq!(ChannelLayout::ambisonic(u16::MAX, []), None);
}

#[test]
fn the_frozen_constants_are_the_layouts_they_claim() {
    assert_eq!(ChannelLayout::MONO.name(), Some("mono"));
    assert_eq!(
        ChannelLayout::MONO.channel_at(0),
        Some(Channel::FrontCenter)
    );
    assert_eq!(ChannelLayout::STEREO.name(), Some("stereo"));
    assert_eq!(
        ChannelLayout::from_name("mono").as_ref(),
        Some(&ChannelLayout::MONO)
    );
    assert_eq!(
        ChannelLayout::from_name("stereo").as_ref(),
        Some(&ChannelLayout::STEREO)
    );
}

#[test]
fn canonicalisation_collapses_what_the_reference_collapses() {
    // Ascending and maskable -> native.
    let l = ChannelLayout::custom([Channel::FrontLeft, Channel::FrontRight]).expect("stereo");
    assert_eq!(l.order, ChannelOrder::Native);
    // Descending -> custom, because the order is part of the layout.
    let l = ChannelLayout::custom([Channel::FrontRight, Channel::FrontLeft]).expect("custom");
    assert!(matches!(l.order, ChannelOrder::Custom(_)));
    // All-unknown -> unspecified.
    let l = ChannelLayout::custom([Channel::Unknown; 3]).expect("unspec");
    assert_eq!(l.order, ChannelOrder::Unspecified);
    // A complete ACN set -> ambisonic.
    let acn = (0..4u16).map(Channel::Ambisonic);
    let l = ChannelLayout::custom(acn).expect("ambisonic");
    assert!(matches!(
        l.order,
        ChannelOrder::Ambisonic { order: 0..=255, .. }
    ));
    // An incomplete one does not.
    let l = ChannelLayout::custom((0..3u16).map(Channel::Ambisonic)).expect("custom");
    assert!(matches!(l.order, ChannelOrder::Custom(_)));
    assert_eq!(ChannelLayout::custom([]), None);
}

#[test]
fn structurally_invalid_layouts_are_reported_not_hidden() {
    // The reference accepts this string and rejects the layout afterwards.
    let l = ChannelLayout::from_name("ambisonic 1+4 channels").expect("parses");
    assert!(!l.is_valid());
    assert_eq!(l.channels, 8);

    // The one place our answer differs from the reference's, and only while the
    // layout is in this rejected state: it materialises the unspecified extras
    // as its `AV_CHAN_NONE` sentinel and prints `ambisonic 3+3 channels
    // (NONE+NONE+NONE)`, where we leave them `UNK`. Pinned so that a change to
    // `channel_at`'s answer for an unspecified layout is a visible one.
    let l = ChannelLayout::from_name("ambisonic 3+3 channels").expect("parses");
    assert!(!l.is_valid());
    assert_eq!(l.channels, 19);
    assert_eq!(l.describe(), "ambisonic 3+3 channels");
    assert_eq!(l.channel_at(16), Some(Channel::Unknown));

    let mut broken = ChannelLayout::STEREO;
    broken.channels = 3;
    assert!(!broken.is_valid());
    assert!(!ChannelLayout::unspecified(0).is_valid());
}

// ------------------------------------------------------------------ property

fn arb_channel() -> impl Strategy<Value = Channel> {
    prop_oneof![
        (0u32..64).prop_filter_map("named", Channel::from_id),
        Just(Channel::Unknown),
        Just(Channel::Unused),
        (0u16..8).prop_map(Channel::Ambisonic),
    ]
}

/// Labels are drawn to straddle the cap and the multi-byte boundary, since that
/// is where `describe`'s fixed point is hardest to hold.
fn arb_label() -> impl Strategy<Value = Option<Label>> {
    prop_oneof![
        4 => Just(None),
        1 => "[A-Za-z0-9 @]{0,20}".prop_map(|s| Label::new(&s)),
        1 => "[éa\u{1f600}]{0,9}".prop_map(|s| Label::new(&s)),
    ]
}

fn arb_entry() -> impl Strategy<Value = (Channel, Option<Label>)> {
    (arb_channel(), arb_label())
}

fn arb_layout() -> impl Strategy<Value = ChannelLayout> {
    prop_oneof![
        (1u64..=u64::MAX).prop_filter_map("mask", ChannelLayout::from_mask),
        (1u32..64).prop_map(ChannelLayout::unspecified),
        prop::collection::vec(arb_entry(), 1..10)
            .prop_filter_map("custom", ChannelLayout::custom_labelled),
        (0u16..4, prop::collection::vec(arb_entry(), 0..3))
            .prop_filter_map("ambisonic", |(o, e)| ChannelLayout::ambisonic_labelled(
                o, e
            )),
    ]
}

proptest! {
    #[test]
    fn describe_round_trips(layout in arb_layout()) {
        let text = layout.describe();
        let again = ChannelLayout::from_name(&text);
        prop_assert_eq!(again.as_ref(), Some(&layout), "describe -> parse lost {}", text);
    }

    #[test]
    fn mask_round_trips(mask in 1u64..=u64::MAX) {
        let layout = ChannelLayout::from_mask(mask).expect("non-zero");
        prop_assert_eq!(layout.mask(), mask);
        prop_assert_eq!(layout.channels, mask.count_ones());
        let hex = ChannelLayout::from_name(&format!("{mask:#x}"));
        let dec = ChannelLayout::from_name(&format!("{mask}"));
        prop_assert_eq!(hex.as_ref(), Some(&layout));
        prop_assert_eq!(dec.as_ref(), Some(&layout));
    }

    #[test]
    fn channel_at_and_index_of_are_inverse(layout in arb_layout()) {
        for i in 0..layout.channels {
            let ch = layout.channel_at(i).expect("in range");
            // `index_of` returns the *first* index carrying the channel, so it
            // agrees with `channel_at` except where a channel repeats.
            let back = layout.index_of(ch);
            if matches!(layout.order, super::ChannelOrder::Unspecified) {
                prop_assert_eq!(back, None);
            } else {
                let j = back.expect("present");
                prop_assert!(j <= i);
                prop_assert_eq!(layout.channel_at(j), Some(ch));
            }
        }
        prop_assert_eq!(layout.channel_at(layout.channels), None);
    }

    #[test]
    fn channel_id_round_trips(id in 0u32..=(i32::MAX as u32)) {
        let ch = Channel::from_id(id).expect("in range");
        prop_assert_eq!(ch.id(), id);
        prop_assert_eq!(Channel::from_name(&ch.to_string()), Some(ch));
    }

    #[test]
    fn arbitrary_text_never_panics(s in ".{0,40}") {
        if let Some(l) = ChannelLayout::from_name(&s) {
            // Whatever came out, it must be self-consistent enough to describe
            // and to index without panicking.
            let _ = l.describe();
            for i in 0..l.channels.min(4096) {
                let _ = l.channel_at(i);
            }
        }
    }

    #[test]
    fn plus_joined_channel_names_never_panic(
        parts in prop::collection::vec("[A-Z@0-9 ]{0,6}", 1..6)
    ) {
        let s = parts.join("+");
        if let Some(l) = ChannelLayout::from_name(&s) {
            prop_assert!(l.channels > 0);
            let _ = l.describe();
        }
    }
}

#[test]
fn labels_survive_and_block_the_collapses_they_cannot_survive() {
    // A label is the difference between a mask and a map.
    let plain = ChannelLayout::from_name("FL+FR").expect("stereo");
    let tagged = ChannelLayout::from_name("FL@Left+FR@Right").expect("labelled");
    assert_eq!(plain.order, ChannelOrder::Native);
    assert!(matches!(tagged.order, ChannelOrder::Custom(_)));
    assert_eq!(tagged.describe(), "2 channels (FL@Left+FR@Right)");
    assert_eq!(tagged.label_at(0).map(Label::as_str), Some("Left"));
    assert_eq!(tagged.label_at(1).map(Label::as_str), Some("Right"));
    assert_eq!(tagged.label_at(2), None);
    assert_eq!(plain.label_at(0), None, "a mask cannot carry a label");

    // It blocks the collapse to unspecified for the same reason.
    let l = ChannelLayout::from_name("UNK@x+UNK").expect("labelled unknown");
    assert!(matches!(l.order, ChannelOrder::Custom(_)));
    assert_eq!(l.describe(), "2 channels (UNK@x+UNK)");
    assert_eq!(
        ChannelLayout::from_name("UNK+UNK").map(|l| l.order),
        Some(ChannelOrder::Unspecified)
    );

    // But not the collapse to ambisonic: a label on an ACN component is
    // discarded, while one on a non-diegetic extra survives.
    let l = ChannelLayout::from_name("AMBI0@z+AMBI1+AMBI2+AMBI3").expect("ambisonic");
    assert_eq!(l.describe(), "ambisonic 1");
    assert_eq!(l.label_at(0), None);
    let l = ChannelLayout::from_name("ambisonic 1+FL@x+FR").expect("ambisonic + extras");
    assert_eq!(l.describe(), "ambisonic 1+2 channels (FL@x+FR)");
    assert_eq!(l.label_at(4).map(Label::as_str), Some("x"));
    assert_eq!(l.label_at(5), None);

    // An empty label is no label: `FL@+FR` is plain stereo.
    assert_eq!(Label::new(""), None);
    assert_eq!(ChannelLayout::from_name("FL@+FR"), Some(plain));
}

#[test]
fn labels_are_truncated_at_the_cap_by_construction() {
    assert_eq!(Label::CAP, 15);
    let kept = Label::new("abcdefghijklmno").expect("exactly the cap");
    assert_eq!(kept.as_str(), "abcdefghijklmno");
    let cut = Label::new("abcdefghijklmnopq").expect("over the cap");
    assert_eq!(cut.as_str(), "abcdefghijklmno");
    // There is no way to build a longer one, which is the point of the type.
    for text in [
        "a",
        "abcdefghijklmnopqrstuvwxyz",
        "ééééééééééé",
        "\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}",
    ] {
        let label = Label::new(text).expect("non-empty");
        assert!(label.as_str().len() <= Label::CAP);
        assert!(text.starts_with(label.as_str()));
        // Idempotent, which is what keeps `describe` a fixed point.
        assert_eq!(Label::new(label.as_str()), Some(label));
    }
}

#[test]
fn the_label_truncation_divergence_is_exactly_what_we_documented() {
    // Pins the size of the one gap that remains. If `describe` ever grows a
    // byte-exact counterpart, these rows should move into `GOLDEN` and this
    // test should go.
    for (input, reference, ours) in LABEL_TRUNCATION_DIVERGENCE {
        let got = ChannelLayout::from_name(input).expect("the string still parses");
        assert_ne!(
            got.describe().as_bytes(),
            reference,
            "{input:?} now matches — move it into GOLDEN"
        );
        assert_eq!(got.describe(), ours, "{input:?} diverges in a new way");
        // The part we do get right: the reference's bytes up to the cut.
        let prefix = reference.get(..reference.len() - 2).expect("longer than 2");
        assert!(got.describe().as_bytes().starts_with(prefix));
        // And the invariant the divergence exists to protect.
        let again = ChannelLayout::from_name(&got.describe()).expect("fixed point");
        assert_eq!(again, got);
    }
}

#[test]
fn layout_stays_small() {
    use core::mem::size_of;

    // WHY THIS BOUND EXISTS, AND WHERE IT IS FELT
    //
    // `ChannelLayout` is embedded *by value* in `FrameData::Audio` in
    // `vaco-frame`, so every audio frame pays its size whether or not the layout
    // is custom — and a frame is constructed on the decode hot path. When this
    // enum briefly held `SmallVec<[ChannelEntry; 8]>` inline, `ChannelLayout`
    // reached 256 bytes and tripped `clippy::large_enum_variant` on
    // `FrameData` — a warning three crates away from the cause, which is
    // exactly the kind of failure this test exists to convert into a local one.
    //
    // The bounds are the measured sizes on a 64-bit target with one word of
    // slack, no more: they are meant to fail on a real widening, and a `<=`
    // keeps them valid on 32-bit, where pointers are half the size.
    //
    // If you need to exceed one of these, that is a decision, not a fix. The
    // trade is written up under "Why a boxed slice and not a `SmallVec`" on
    // `ChannelMap`; read it first, and re-run the measurement rather than
    // guessing at a new number.
    assert!(size_of::<Label>() <= 16, "Label: {}", size_of::<Label>());
    assert!(
        size_of::<Option<Label>>() == size_of::<Label>(),
        "the `NonZeroU8` niche is gone, so every entry grew: Option<Label> is {}",
        size_of::<Option<Label>>()
    );
    assert!(
        size_of::<ChannelEntry>() <= 24,
        "ChannelEntry: {}",
        size_of::<ChannelEntry>()
    );
    assert!(
        size_of::<ChannelOrder>() <= 24,
        "ChannelOrder: {}",
        size_of::<ChannelOrder>()
    );
    assert!(
        size_of::<ChannelLayout>() <= 40,
        "ChannelLayout: {}",
        size_of::<ChannelLayout>()
    );
}

#[test]
fn the_common_layouts_do_not_allocate() {
    // The other half of the boxed-slice trade: `Native` and `Unspecified` carry
    // no map at all, and an ambisonic layout with no extras boxes an empty
    // slice, which does not allocate either. Only a genuinely custom layout
    // reaches the allocator, and that is the case already off the hot path.
    for layout in [
        ChannelLayout::MONO,
        ChannelLayout::STEREO,
        ChannelLayout::from_name("7.1").expect("7.1"),
        ChannelLayout::unspecified(6),
        ChannelLayout::from_name("ambisonic 3").expect("ambisonic"),
    ] {
        assert!(
            !matches!(layout.order, ChannelOrder::Custom(_)),
            "{layout} should not need a map"
        );
        if let ChannelOrder::Ambisonic { extra, .. } = &layout.order {
            assert!(extra.is_empty());
        }
        // Cloning one is a memcpy of a handful of words.
        assert_eq!(layout.clone(), layout);
    }
}
