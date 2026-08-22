//! The three specifier grammars, against arbitrary text.
//!
//! A stream specifier arrives verbatim from `argv`, so it is untrusted input in
//! the plainest sense (D6). It is also parsed *before* anything is validated,
//! which makes it the earliest reachable code in either binary.
//!
//! Beyond "does not panic", two invariants are asserted:
//!
//! * **`canonical()` is an inverse of `parse`.** A rendering that fails to
//!   reparse, or reparses to something else, means a field is being lost —
//!   which would silently change which streams an option applies to.
//! * **Matching is total and consistent.** `select` must stay within the stream
//!   set and must agree with `matches` for every index, on a stream set the
//!   specifier was not written for.
//! fuzz-crate: vaco-cli-core

#![no_main]
use libfuzzer_sys::fuzz_target;
use vaco_cli_core::{
    Disposition, MapSpec, MatchCtx, MetadataSpecifier, StreamInfo, StreamSpecifier,
};
use vaco_core::MediaType;

fn fixture() -> Vec<StreamInfo> {
    let mk = |index: u32, media: Option<MediaType>, disposition, codec_known| StreamInfo {
        index,
        id: i64::from(index) * 3 - 1,
        media_type: media,
        disposition,
        tags: {
            let mut d = vaco_core::Dict::new();
            d.set("LANGUAGE", "eng");
            d.set("title", "x");
            d
        },
        codec_known,
        width: if index % 2 == 0 { 16 } else { 0 },
        height: 16,
        sample_rate: 48_000,
    };
    vec![
        mk(0, Some(MediaType::Video), Disposition::NONE, true),
        mk(1, Some(MediaType::Video), Disposition::ATTACHED_PIC, true),
        mk(2, Some(MediaType::Audio), Disposition::DEFAULT, true),
        mk(3, Some(MediaType::Audio), Disposition::NONE, false),
        mk(4, Some(MediaType::Subtitle), Disposition::FORCED, true),
        mk(5, None, Disposition::NONE, false),
    ]
}

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else {
        return;
    };

    // Metadata and map specifiers must be total; they have no round trip that
    // holds for every input (a metadata specifier deliberately swallows tails).
    let _ = MetadataSpecifier::parse(s);
    if let Ok(m) = MapSpec::parse(s) {
        let rendered = m.to_string();
        let back = MapSpec::parse(&rendered)
            .unwrap_or_else(|e| panic!("map {s:?} rendered {rendered:?} which fails: {e}"));
        assert_eq!(back, m, "map round-trip changed meaning via {rendered:?}");
    }

    let Ok(spec) = StreamSpecifier::parse(s) else {
        return;
    };

    let rendered = spec.canonical();
    let back = StreamSpecifier::parse(&rendered)
        .unwrap_or_else(|e| panic!("{s:?} rendered {rendered:?} which fails to parse: {e}"));
    assert_eq!(back, spec, "canonical round-trip changed meaning via {rendered:?}");

    let streams = fixture();
    let ctx = MatchCtx::streams(&streams);
    let picked = spec.select(&ctx);

    assert!(
        picked.windows(2).all(|w| w.first() < w.last()),
        "selection is not in strictly increasing container order: {picked:?}"
    );
    for i in &picked {
        assert!(
            (*i as usize) < streams.len(),
            "selection {i} is outside the stream set"
        );
    }
    if spec.index.is_some() {
        assert!(picked.len() <= 1, "an index token selected {picked:?}");
    }
    if spec.is_empty() {
        assert_eq!(picked.len(), streams.len(), "the empty specifier must match all");
    }
    for i in 0..streams.len() as u32 {
        assert_eq!(
            spec.matches(&ctx, i),
            picked.contains(&i),
            "matches/select disagree for stream {i} of {s:?}"
        );
    }
});
