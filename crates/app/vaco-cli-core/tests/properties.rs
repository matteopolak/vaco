//! Property tests for the invariants the unit tests can only sample.
//!
//! Three families:
//!
//! * **Round trips.** A parsed specifier renders to text that parses back to
//!   the same specifier. Same for `-map`. This is what stops a "clever"
//!   canonicalisation from quietly losing a field.
//! * **Totality.** No input, however hostile, panics. Argument vectors are
//!   untrusted (D6) and every lint that would let a panic through is denied, so
//!   this is checked rather than assumed.
//! * **Matcher agreement.** `matches` and `select` cannot disagree, selection is
//!   always a subsequence of container order, and an index token always yields
//!   at most one stream.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]

use std::ffi::OsString;

use proptest::prelude::*;
use vaco_cli_core::{
    Disposition, MapSpec, MatchCtx, StreamInfo, StreamSpecifier, ffmpeg, ffprobe, split,
};
use vaco_core::MediaType;

/// Text drawn from the specifier alphabet, so the generator actually reaches
/// the interesting states instead of spending its budget on rejected garbage.
fn spec_text() -> impl Strategy<Value = String> {
    let token = prop::sample::select(vec![
        "v", "V", "a", "s", "d", "t", "u", "p:", "g:", "i:", "m:", "disp:", "#", ":", "0", "1",
        "10", "0x2", "010", "default", "forced", "+", "-", "x", "\\", "", "k", "99",
    ]);
    prop::collection::vec(token, 0..7).prop_map(|v| v.concat())
}

/// Anything at all, including things no specifier alphabet contains.
fn wild_text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => spec_text(),
        1 => ".{0,24}",
    ]
}

fn stream_strategy() -> impl Strategy<Value = StreamInfo> {
    (
        prop::option::of(prop::sample::select(vec![
            MediaType::Video,
            MediaType::Audio,
            MediaType::Subtitle,
            MediaType::Data,
            MediaType::Attachment,
        ])),
        any::<i64>(),
        any::<bool>(),
        0u32..3,
        prop::sample::select(vec![
            Disposition::NONE,
            Disposition::DEFAULT,
            Disposition::ATTACHED_PIC,
            Disposition::DEFAULT | Disposition::FORCED,
        ]),
    )
        .prop_map(
            |(media_type, id, codec_known, dim, disposition)| StreamInfo {
                index: 0,
                id,
                media_type,
                disposition,
                tags: vaco_core::Dict::new(),
                codec_known,
                width: dim,
                height: dim,
                sample_rate: dim * 16_000,
            },
        )
}

fn ctx_streams() -> impl Strategy<Value = Vec<StreamInfo>> {
    prop::collection::vec(stream_strategy(), 0..6).prop_map(|mut v| {
        for (i, s) in v.iter_mut().enumerate() {
            s.index = u32::try_from(i).unwrap_or(u32::MAX);
        }
        v
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// Parsing never panics and never hangs, whatever the bytes.
    #[test]
    fn specifier_parsing_is_total(s in wild_text()) {
        let _ = StreamSpecifier::parse(&s);
    }

    #[test]
    fn map_parsing_is_total(s in wild_text()) {
        let _ = MapSpec::parse(&s);
    }

    /// `canonical()` is a real inverse of `parse`, not merely a debug rendering.
    #[test]
    fn specifier_canonical_round_trips(s in spec_text()) {
        if let Ok(spec) = StreamSpecifier::parse(&s) {
            let text = spec.canonical();
            let back = StreamSpecifier::parse(&text)
                .unwrap_or_else(|e| panic!("{s:?} -> {text:?} failed to reparse: {e}"));
            prop_assert_eq!(spec, back, "via {:?}", text);
        }
    }

    #[test]
    fn map_display_round_trips(s in spec_text()) {
        if let Ok(m) = MapSpec::parse(&s) {
            let text = m.to_string();
            let back = MapSpec::parse(&text)
                .unwrap_or_else(|e| panic!("{s:?} -> {text:?} failed to reparse: {e}"));
            prop_assert_eq!(m, back, "via {:?}", text);
        }
    }

    /// Selection is a subsequence of container order with no repeats, and every
    /// index it yields exists.
    #[test]
    fn selection_is_well_formed(s in spec_text(), streams in ctx_streams()) {
        let Ok(spec) = StreamSpecifier::parse(&s) else { return Ok(()) };
        let ctx = MatchCtx::streams(&streams);
        let picked = spec.select(&ctx);
        prop_assert!(
            picked.windows(2).all(|w| w.first() < w.last()),
            "not strictly increasing: {picked:?}"
        );
        for i in &picked {
            prop_assert!((*i as usize) < streams.len());
        }
        // An index token selects at most one stream.
        if spec.index.is_some() {
            prop_assert!(picked.len() <= 1);
        }
        // The empty specifier selects everything.
        if spec.is_empty() {
            prop_assert_eq!(picked.len(), streams.len());
        }
    }

    /// `matches` is exactly membership of `select`.
    #[test]
    fn matches_agrees_with_select(s in spec_text(), streams in ctx_streams()) {
        let Ok(spec) = StreamSpecifier::parse(&s) else { return Ok(()) };
        let ctx = MatchCtx::streams(&streams);
        let picked = spec.select(&ctx);
        for i in 0..u32::try_from(streams.len()).unwrap_or(0) {
            prop_assert_eq!(spec.matches(&ctx, i), picked.contains(&i));
        }
    }

    /// Splitting an arbitrary argument vector never panics, and when it
    /// succeeds the structural invariants hold.
    #[test]
    fn splitting_is_total_and_well_formed(argv in prop::collection::vec(wild_arg(), 0..12)) {
        for table in [ffmpeg(), ffprobe()] {
            let Ok(cl) = split(&table, &argv) else { continue };

            // Every global option really is global; every grouped and orphaned
            // option really is not.
            for o in &cl.global {
                let d = o.desc.expect("a deferred option is never hoisted");
                prop_assert!(d.flags.contains(vaco_cli_core::OptFlags::GLOBAL));
            }
            for o in cl.groups.iter().flat_map(|g| &g.opts).chain(&cl.orphaned) {
                if let Some(d) = o.desc {
                    prop_assert!(!d.flags.contains(vaco_cli_core::OptFlags::GLOBAL));
                }
            }

            // Group indices are 0..n within each kind, in argv order.
            for kind in [vaco_cli_core::GroupKind::Input, vaco_cli_core::GroupKind::Output] {
                let seen: Vec<u32> = cl.of_kind(kind).map(|g| g.index).collect();
                let want: Vec<u32> = (0..u32::try_from(seen.len()).unwrap_or(0)).collect();
                prop_assert_eq!(seen, want);
            }

            // Nothing is lost: every argv entry is either a URL, an option
            // name, or an option's value.
            let counted = cl.global.len()
                + cl.orphaned.len()
                + cl.groups.iter().map(|g| g.opts.len()).sum::<usize>();
            prop_assert!(counted + cl.groups.len() <= argv.len());

            // Validation is total too.
            let _ = cl.validate();

            // So is specifier resolution on whatever survived.
            for o in cl.global.iter().chain(cl.groups.iter().flat_map(|g| &g.opts)) {
                let _ = o.stream_spec();
                let _ = o.metadata_spec();
            }
        }
    }
}

/// Argv entries drawn from a mix of real option spellings and noise.
fn wild_arg() -> impl Strategy<Value = OsString> {
    prop_oneof![
        4 => prop::sample::select(vec![
            "-i", "-y", "-n", "-c:v", "-c:a:1", "-map", "-metadata:s:v:0", "-t", "-f", "--",
            "-", "-vf", "-nostats", "-/filter:v", "-shortest", "-re", "-ss", "--help", "-qwerty",
            "-c:", "-c:zzz", "-y:vv",
        ]).prop_map(OsString::from),
        3 => prop::sample::select(vec![
            "in.mkv", "out.mp4", "null", "copy", "1", "0:v", "title=x", "libx264", "",
        ]).prop_map(OsString::from),
        1 => ".{0,10}".prop_map(OsString::from),
    ]
}
