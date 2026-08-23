//! Parameters, capabilities, profiles and levels.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use vaco_codec_core::params::{
    AudioParameters, LevelConstraints, LevelEntry, LevelQuery, ProfileEntry, VideoParameters,
};
use vaco_codec_core::{
    Caps, CodecId, CodecParameters, CodecProperties, DecoderDesc, Level, LevelTable, Profile,
    ProfileTable, Threading,
};
use vaco_core::{MediaType, Rational};
use vaco_limits::{Budget, Limits};

#[test]
fn codec_identity_round_trips_through_its_name() {
    for id in CodecId::all() {
        assert_eq!(CodecId::from_name(id.name()), Some(id));
        assert!(!id.long_name().is_empty());
    }
    assert_eq!(CodecId::from_name("AV1"), Some(CodecId::Av1));
    assert_eq!(CodecId::from_name("nope"), None);
    assert_eq!(CodecId::Av1.media_type(), MediaType::Video);
    assert_eq!(CodecId::Opus.media_type(), MediaType::Audio);
    assert!(CodecId::H264.properties().reorders());
    assert!(CodecId::Png.properties().is_intra_only());
    assert!(
        CodecId::Flac
            .properties()
            .contains(CodecProperties::LOSSLESS)
    );
}

/// Which codecs need `frame_rate / ticks_per_frame` rather than `frame_rate`
/// alone when a demuxer has to synthesise a packet duration from the codec's
/// own rate. Measured (issue #632 part 1) on real 25/24/30 fps encodes: H.264
/// and MPEG-1 video always report exactly double the true rate; MPEG-2 video
/// and MPEG-4 part 2 — despite both being interlace-capable, the guess that
/// would have been wrong — report the true rate directly. Everything else
/// defaults to 1, and this test pins that default alongside the two measured
/// exceptions so a new codec cannot silently join the halved group.
#[test]
fn ticks_per_frame_is_two_only_for_the_measured_codecs() {
    assert_eq!(CodecId::H264.ticks_per_frame(), 2);
    assert_eq!(CodecId::Mpeg1video.ticks_per_frame(), 2);
    assert_eq!(CodecId::Mpeg2video.ticks_per_frame(), 1);
    assert_eq!(CodecId::Mpeg4.ticks_per_frame(), 1);
    assert_eq!(CodecId::Hevc.ticks_per_frame(), 1);
    assert_eq!(CodecId::Av1.ticks_per_frame(), 1);
    for id in CodecId::all() {
        assert!(
            id.ticks_per_frame() == 1 || matches!(id, CodecId::H264 | CodecId::Mpeg1video),
            "{id:?} claims a ticks_per_frame nobody measured"
        );
    }
}

/// Every row of the table agrees with `ffmpeg -codecs`.
///
/// The table was **generated** by probing that listing, so checking it against
/// the same listing is not circular in the way it first looks: the generator
/// ran once, by hand, and its output was pasted in. Nothing re-runs it. This is
/// what catches a row edited afterwards, a variant added by hand, or a change
/// in the reference between versions.
///
/// Two rules the generator had to learn, both re-asserted here because they are
/// exactly what a hand edit would get wrong:
///
/// - `-codecs` appends `(decoders: …)` / `(encoders: …)` to a long name when the
///   codec has differently-named implementations, and that suffix is **not**
///   part of `codec_long_name`. `subrip` is the witness — the listing says
///   `"SubRip subtitle (decoders: srt subrip)"` and `ffprobe` prints
///   `"SubRip subtitle"`.
/// - DTS's codec is `dts` with the long name `"DCA (DTS Coherent Acoustics)"`,
///   while its decoder is `dca`. `-h decoder=dts` gives the wrong string.
///
/// Skipped rather than failed when `ffmpeg` is absent: CI has it, a contributor
/// may not, and a test that cannot run is not a test that failed.
#[test]
fn the_codec_table_agrees_with_the_reference() {
    let Ok(out) = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-codecs"])
        .env("LC_ALL", "C")
        .output()
    else {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    };
    let listing = String::from_utf8_lossy(&out.stdout);

    let mut reference: std::collections::BTreeMap<&str, String> = std::collections::BTreeMap::new();
    for line in listing.lines() {
        // ` DEVILS name  Long name`, six flag columns then two fields.
        let Some(rest) = line.strip_prefix(' ') else {
            continue;
        };
        let (flags, rest) = rest.split_at(rest.char_indices().nth(6).map_or(0, |(i, _)| i));
        if flags.len() != 6 || !flags.chars().all(|c| "DEVASIL.S-".contains(c)) {
            continue;
        }
        let mut it = rest.split_whitespace();
        let Some(name) = it.next() else { continue };
        let long = rest[rest.find(name).map_or(0, |i| i + name.len())..].trim();
        // Strip the listing's own annotation; see the doc comment.
        let long = long.split(" (decoders:").next().unwrap_or(long);
        let long = long.split(" (encoders:").next().unwrap_or(long);
        reference.insert(name, long.to_owned());
    }
    assert!(
        reference.len() > 100,
        "parsed only {} rows from -codecs; the listing format changed",
        reference.len()
    );

    let mut wrong = Vec::new();
    for id in CodecId::all() {
        let Some(long) = reference.get(id.name()) else {
            // A name the reference does not have is a different question —
            // `-pix_fmts` found one of those in vaco-pixfmt — but it is not
            // what this test is for, and several of ours are deliberate.
            continue;
        };
        if long != id.long_name() {
            wrong.push(format!(
                "  {}: ours {:?}, reference {:?}",
                id.name(),
                id.long_name(),
                long
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "long names disagree:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn capability_names_round_trip_and_print() {
    for (cap, name) in [
        (Caps::DELAY, "delay"),
        (Caps::SUBFRAMES, "subframes"),
        (Caps::PATENT_ENCUMBERED, "patent_encumbered"),
    ] {
        assert_eq!(Caps::from_cli_name(name), Some(cap));
        assert_eq!(cap.names().collect::<Vec<_>>(), vec![name]);
    }
    let c = Caps::DELAY | Caps::SUBFRAMES;
    assert_eq!(c.to_string(), "delay+subframes");
    assert_eq!(Caps::empty().to_string(), "none");
    assert!(c.needs_drain());
    assert!(c.may_expand());
}

#[test]
fn the_patent_flag_is_what_ci_asserts_on() {
    const SAFE: DecoderDesc = DecoderDesc {
        name: "av1",
        long_name: "AV1",
        id: CodecId::Av1,
        media_type: MediaType::Video,
        caps: Caps::DELAY,
        supported_rates: &[],
    };
    const ENCUMBERED: DecoderDesc = DecoderDesc {
        name: "aac",
        long_name: "AAC",
        id: CodecId::Aac,
        media_type: MediaType::Audio,
        caps: Caps::PATENT_ENCUMBERED,
        supported_rates: &[],
    };
    assert!(SAFE.is_default_build_safe());
    assert!(!ENCUMBERED.is_default_build_safe());
    assert!(ENCUMBERED.caps.opt_in_required().is_patent_encumbered());
    // An empty rate table means unconstrained.
    assert!(SAFE.supports_rate(Rational::new(30, 1)));
}

#[test]
fn parameters_reject_inconsistent_media_types() {
    let mut p = CodecParameters::video();
    assert!(p.check_consistent().is_ok());
    p.audio = Some(AudioParameters::default());
    assert!(p.check_consistent().is_err());

    let mut q = CodecParameters::new(MediaType::Audio);
    q.video = Some(VideoParameters::default());
    assert!(q.check_consistent().is_err());
}

#[test]
fn parameters_are_validated_against_a_budget() {
    let budget = Budget::new(Limits::strict());
    let mut p = CodecParameters::video();
    if let Some(v) = p.video.as_mut() {
        v.width = 1920;
        v.height = 1080;
    }
    assert!(p.validate(&budget).is_ok());
    if let Some(v) = p.video.as_mut() {
        v.width = 1 << 20;
    }
    assert!(p.validate(&budget).is_err());
}

#[test]
fn the_container_wins_and_the_parser_only_fills_gaps() {
    let mut container = CodecParameters::video().with_codec(CodecId::H264);
    if let Some(v) = container.video.as_mut() {
        v.width = 1920;
        v.height = 1080;
    }
    let mut parsed = CodecParameters::video();
    if let Some(v) = parsed.video.as_mut() {
        v.width = 1440;
        v.height = 1080;
        v.has_b_frames = 2;
        v.frame_rate = Rational::new(30000, 1001);
    }
    container.fill_from(&parsed);
    let v = container.video.as_ref().unwrap();
    assert_eq!(v.width, 1920, "the container's own value must win");
    assert_eq!(
        v.has_b_frames, 2,
        "what the container did not say is filled in"
    );
    assert_eq!(v.frame_rate, Rational::new(30000, 1001));
}

const PROFILES: &[ProfileEntry] = &[
    ProfileEntry {
        profile: Profile::new(0, "Main"),
        subsumes: &[],
    },
    ProfileEntry {
        profile: Profile::new(1, "High"),
        subsumes: &[0],
    },
    ProfileEntry {
        profile: Profile::new(2, "Professional"),
        subsumes: &[0, 1],
    },
];

#[test]
fn profiles_resolve_by_name_and_subsume_downwards() {
    let t = ProfileTable(PROFILES);
    assert_eq!(t.from_name("high"), Some(Profile::new(1, "High")));
    assert_eq!(t.from_value(2).map(|p| p.name), Some("Professional"));
    assert!(t.subsumes(Profile::new(2, "Professional"), Profile::new(0, "Main")));
    assert!(t.subsumes(Profile::new(1, "High"), Profile::new(1, "High")));
    assert!(!t.subsumes(Profile::new(0, "Main"), Profile::new(1, "High")));
}

const fn level(raw: i32, name: &'static str, w: u32, h: u32, luma: u64, rate: u64) -> LevelEntry {
    LevelEntry {
        level: Level(raw),
        name,
        constraints: LevelConstraints {
            max_luma_picture_size: luma,
            max_luma_sample_rate: rate,
            max_bitrate_kbps: 0,
            max_dpb_frames: 16,
            max_h_size: w,
            max_v_size: h,
            max_tiles: 0,
            max_tile_cols: 0,
        },
    }
}

const LEVELS: &[LevelEntry] = &[
    level(30, "3.0", 720, 576, 414_720, 10_368_000),
    level(40, "4.0", 2048, 1152, 2_097_152, 62_914_560),
    level(50, "5.0", 4096, 2048, 8_912_896, 267_386_880),
];

#[test]
fn the_smallest_level_that_fits_is_the_one_chosen() {
    let t = LevelTable(LEVELS);
    let sd = LevelQuery {
        width: 720,
        height: 576,
        luma_sample_rate: 10_368_000,
        dpb_frames: 4,
        ..Default::default()
    };
    assert_eq!(t.smallest_for(&sd), Some(Level(30)));

    let hd = LevelQuery {
        width: 1920,
        height: 1080,
        luma_sample_rate: 62_208_000,
        dpb_frames: 4,
        ..Default::default()
    };
    assert_eq!(t.smallest_for(&hd), Some(Level(40)));
    assert!(!t.admits(Level(30), &hd));
    assert!(t.admits(Level(50), &hd));

    let huge = LevelQuery {
        width: 16384,
        height: 8192,
        luma_sample_rate: u64::MAX,
        ..Default::default()
    };
    assert_eq!(t.smallest_for(&huge), None);

    assert_eq!(t.name(Level(40)), Some("4.0"));
    assert_eq!(t.from_name("5.0"), Some(Level(50)));
    assert_eq!(t.constraints(Level(99)), None);
}

#[test]
fn threading_declarations_must_match_the_capabilities() {
    let frame = Threading::Frame {
        max_frames: 4,
        delay: 3,
    };
    assert!(frame.is_consistent_with(Caps::FRAME_THREADS));
    assert!(!frame.is_consistent_with(Caps::SLICE_THREADS));
    assert_eq!(frame.max_frames(), 4);
    assert_eq!(frame.delay(), 3);
    assert_eq!(frame.max_jobs(), 1);

    // Determinism is a contract: a thread-count clamp only ever narrows.
    assert_eq!(frame.clamped_to(1), Threading::None);
    assert_eq!(
        frame.clamped_to(2),
        Threading::Frame {
            max_frames: 2,
            delay: 3
        }
    );
    assert_eq!(frame.clamped_to(64), frame);
    assert_eq!(Threading::None.max_frames(), 1);
}

// ---------------------------------------- what a parser is allowed to supply

/// The colour description merges **per property**, not whole-struct.
///
/// MP4's `colr` box states primaries, transfer and matrix but has no chroma
/// siting at all, while the H.264 VUI beside it does. Replacing the block only
/// when it is entirely default would leave `chroma_location=unspecified` on
/// every such file — measured, 9 of the corpus's 180 divergences were exactly
/// that. The container still wins wherever it stated something.
#[test]
fn colour_merges_property_by_property_and_the_container_still_wins() {
    use vaco_color::{ChromaLocation, ColorInfo, ColorPrimaries, MatrixCoefficients};

    let mut container = VideoParameters {
        color: ColorInfo {
            primaries: ColorPrimaries::Bt709,
            ..ColorInfo::default()
        },
        ..VideoParameters::default()
    };
    let bitstream = VideoParameters {
        color: ColorInfo {
            // Disagrees with the container: the container must win.
            primaries: ColorPrimaries::Bt2020,
            // The container left these unset: the parser must fill them.
            matrix: MatrixCoefficients::Bt709,
            chroma_location: ChromaLocation::Left,
            ..ColorInfo::default()
        },
        ..VideoParameters::default()
    };
    container.fill_from(&bitstream);
    assert_eq!(container.color.primaries, ColorPrimaries::Bt709);
    assert_eq!(container.color.matrix, MatrixCoefficients::Bt709);
    assert_eq!(container.color.chroma_location, ChromaLocation::Left);
}

/// `bits_per_raw_sample` and `nal_length_size` fill like every other field:
/// only where the container left a hole.
#[test]
fn the_two_new_video_fields_fill_only_where_unset() {
    let mut container = VideoParameters {
        bits_per_raw_sample: Some(10),
        ..VideoParameters::default()
    };
    container.fill_from(&VideoParameters {
        bits_per_raw_sample: Some(8),
        nal_length_size: Some(4),
        ..VideoParameters::default()
    });
    assert_eq!(container.bits_per_raw_sample, Some(10));
    assert_eq!(container.nal_length_size, Some(4));

    // `Some(0)` is a *value*, not an absence: it is what an Annex B stream
    // reports, and `is_avc=false` depends on being able to tell it from `None`.
    let mut annexb = VideoParameters::default();
    annexb.fill_from(&VideoParameters {
        nal_length_size: Some(0),
        ..VideoParameters::default()
    });
    assert_eq!(annexb.nal_length_size, Some(0));
}
