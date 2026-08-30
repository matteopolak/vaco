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
    Caps, CodecId, CodecParameters, CodecProperties, Decoder, DecoderDesc, Level, LevelTable,
    Profile, ProfileTable, Threading,
};
use vaco_core::{MediaType, Rational};
use vaco_limits::{Budget, Limits};

/// Never called: [`DecoderDesc::make`] just needs a valid function pointer for
/// these descriptors to exist, not a working decoder.
fn unbuilt_decoder(_: Limits) -> Box<dyn Decoder> {
    panic!("test descriptor; not meant to be built")
}

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

/// Samples per frame, for the handful of codecs whose frame size is fixed by
/// the format rather than stated per file. Measured against the reference on
/// an AAC-in-MPEG-TS fixture: the audio stream's true frame is 1024 samples
/// at 44100 Hz, and the reference's `duration_ts` is exactly that much
/// longer than the last observed packet's own PTS — the gap
/// `vaco-demux-mpegts`'s `end_pts` closes with this value.
#[test]
fn fixed_frame_size_is_only_stated_for_the_measured_codecs() {
    assert_eq!(CodecId::Aac.fixed_frame_size(), Some(1024));
    assert_eq!(CodecId::Mp3.fixed_frame_size(), Some(1152));
    assert_eq!(CodecId::Ac3.fixed_frame_size(), Some(1536));
    assert_eq!(CodecId::Eac3.fixed_frame_size(), Some(1536));
    // LATM/LOAS framing can multiplex more than one access unit per logical
    // frame, so the plain-AAC answer does not transfer.
    assert_eq!(CodecId::AacLatm.fixed_frame_size(), None);
    assert_eq!(CodecId::Opus.fixed_frame_size(), None);
    assert_eq!(CodecId::Vorbis.fixed_frame_size(), None);
    assert_eq!(CodecId::Flac.fixed_frame_size(), None);
    assert_eq!(CodecId::Pcm.fixed_frame_size(), None);
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
/// One row of `ffmpeg -codecs`, parsed once and compared against every
/// dimension [`CodecId`] states: `name`/`long_name` are read here exactly as
/// the reference prints them, deliberately **without** `-bitexact` — that
/// flag suppresses `*_long_name` on `ffprobe`'s *per-stream* output
/// (`-show_streams`), and has no effect on this static capability listing at
/// all (measured: `ffmpeg -bitexact -hide_banner -codecs` and the same
/// command without it produce byte-identical rows). A probe that instead
/// compares a real decoded/muxed file's `-show_streams` output against the
/// reference generally *should* pass `-bitexact` on both sides — this one
/// does not need to, because nothing here opens a stream.
struct RefCodec<'a> {
    media: MediaType,
    intra_only: bool,
    lossy: bool,
    lossless: bool,
    long_name: &'a str,
}

/// Names in [`CodecId::all`] that `ffmpeg -codecs` does not list at all —
/// excluded explicitly, not silently skipped, so a name that is *supposed*
/// to be absent cannot quietly stop being checked if it later gains a real
/// reference codec of the same spelling.
///
/// `"pcm"` is this crate's own generic bucket for a family the reference
/// only ever names specifically (`pcm_s16le`, `pcm_alaw`, …); no
/// `codec_name=pcm` exists to compare against.
const NOT_IN_REFERENCE: &[&str] = &["pcm"];

/// Ids whose flags this pass measured as disagreeing with the reference and
/// deliberately left alone, with the reason — not silently accepted, and not
/// blindly forced to match either. Each is a case where matching the
/// reference's raw I/L/S columns would mean asserting something this
/// project does not actually believe:
///
/// * `subrip`/`mov_text`: the reference does not apply the lossy/lossless/
///   intra vocabulary to text subtitle codecs at all (both print `..S...`
///   with no `I`/`L`/`S` — wait, no flags whatsoever in those three
///   columns), so there is nothing to "agree" with; marking a text format
///   trivially intra-only and lossless is this project's own considered
///   modelling choice, not a measurement it could get wrong.
/// * `wrapped_avframe`: an internal passthrough pseudo-codec, not a real
///   coded format: `-codecs` does not flag it intra-only, but "is this
///   frame independently decodable" is not a meaningful question for a
///   pass-through, so keeping the flag was not treated as a bug worth
///   reverting on a hunch.
/// * `png`, `h264`, `hevc`, `av1`: `-codecs` marks these `L` **and** `S`
///   (both lossy- and lossless-capable) or, for `png`, no `I` at all
///   (PNG's own animated form, APNG, can inter-frame-delta like GIF, which
///   this table already does not call intra-only). Whether a two-bit
///   lossy/lossless/intra summary should grow a "both" state for the video
///   codecs, and whether PNG's animation capability should cost it
///   `INTRA_ONLY`, are real modelling questions this pass did not have the
///   standing to answer unilaterally — recorded rather than guessed at.
const KNOWN_PROPERTY_DIVERGENCES: &[&str] = &[
    "subrip",
    "mov_text",
    "wrapped_avframe",
    "png",
    "h264",
    "hevc",
    "av1",
];

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

    let mut reference: std::collections::BTreeMap<&str, RefCodec<'_>> =
        std::collections::BTreeMap::new();
    for line in listing.lines() {
        // ` DEVILS name  Long name`, six flag columns then two fields.
        let Some(rest) = line.strip_prefix(' ') else {
            continue;
        };
        let (flags, rest) = rest.split_at(rest.char_indices().nth(6).map_or(0, |(i, _)| i));
        if flags.len() != 6 || !flags.chars().all(|c| "DEVASDTIL.S-".contains(c)) {
            continue;
        }
        let mut it = rest.split_whitespace();
        let Some(name) = it.next() else { continue };
        let long = rest[rest.find(name).map_or(0, |i| i + name.len())..].trim();
        // Strip the listing's own annotation; see the doc comment.
        let long = long.split(" (decoders:").next().unwrap_or(long);
        let long = long.split(" (encoders:").next().unwrap_or(long);
        let Some(media) = (match flags.as_bytes()[2] {
            b'V' => Some(MediaType::Video),
            b'A' => Some(MediaType::Audio),
            b'S' => Some(MediaType::Subtitle),
            b'D' => Some(MediaType::Data),
            b'T' => Some(MediaType::Attachment),
            _ => None,
        }) else {
            continue;
        };
        reference.insert(
            name,
            RefCodec {
                media,
                intra_only: flags.as_bytes()[3] == b'I',
                lossy: flags.as_bytes()[4] == b'L',
                lossless: flags.as_bytes()[5] == b'S',
                long_name: long,
            },
        );
    }
    assert!(
        reference.len() > 100,
        "parsed only {} rows from -codecs; the listing format changed",
        reference.len()
    );

    let mut wrong = Vec::new();
    for id in CodecId::all() {
        let name = id.name();
        let known_divergence = KNOWN_PROPERTY_DIVERGENCES.contains(&name);
        match reference.get(name) {
            None => {
                if !NOT_IN_REFERENCE.contains(&name) {
                    wrong.push(format!(
                        "  {name}: not in `ffmpeg -codecs` and not in NOT_IN_REFERENCE"
                    ));
                }
            }
            Some(r) => {
                if NOT_IN_REFERENCE.contains(&name) {
                    wrong.push(format!(
                        "  {name}: listed in NOT_IN_REFERENCE, but the reference does have it"
                    ));
                    continue;
                }
                if r.long_name != id.long_name() {
                    wrong.push(format!(
                        "  {name}: long_name ours {:?}, reference {:?}",
                        id.long_name(),
                        r.long_name
                    ));
                }
                if r.media != id.media_type() {
                    wrong.push(format!(
                        "  {name}: media_type ours {:?}, reference {:?}",
                        id.media_type(),
                        r.media
                    ));
                }
                let props = id.properties();
                let flag_wrong = r.intra_only != props.contains(CodecProperties::INTRA_ONLY)
                    || r.lossy != props.contains(CodecProperties::LOSSY)
                    || r.lossless != props.contains(CodecProperties::LOSSLESS);
                if flag_wrong && !known_divergence {
                    wrong.push(format!(
                        "  {name}: properties ours {:?} (I={} L={} S={}), reference I={} L={} S={}",
                        props,
                        props.contains(CodecProperties::INTRA_ONLY),
                        props.contains(CodecProperties::LOSSY),
                        props.contains(CodecProperties::LOSSLESS),
                        r.intra_only,
                        r.lossy,
                        r.lossless,
                    ));
                } else if !flag_wrong && known_divergence {
                    wrong.push(format!(
                        "  {name}: listed in KNOWN_PROPERTY_DIVERGENCES, but properties now agree — remove it from the list"
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "codec table disagrees with the reference:\n{}",
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
        make: unbuilt_decoder,
    };
    const ENCUMBERED: DecoderDesc = DecoderDesc {
        name: "aac",
        long_name: "AAC",
        id: CodecId::Aac,
        media_type: MediaType::Audio,
        caps: Caps::PATENT_ENCUMBERED,
        supported_rates: &[],
        make: unbuilt_decoder,
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

/// A legitimately large frame in a *known* pixel format must be charged its
/// real bytes per pixel, not the flat 4-bytes-per-pixel fallback `validate`
/// uses when the format is not yet known.
///
/// Regression: before this fix, `validate` charged every video stream 4
/// bytes per pixel regardless of `VideoParameters::format`. At 2732x1536, a
/// real `yuv420p` frame is ~8.4 MB (2 bytes/pixel) but the flat charge
/// (16.8 MB) crosses `Limits::strict`'s 16 MiB `max_frame_bytes` cap —
/// exactly the false-rejection shape this session's fix addresses.
#[test]
fn a_known_small_pixel_format_is_charged_its_real_bytes_per_pixel() {
    let budget = Budget::new(Limits::strict());
    let mut p = CodecParameters::video();
    let Some(v) = p.video.as_mut() else {
        panic!("CodecParameters::video() must carry video parameters");
    };
    v.width = 2732;
    v.height = 1536;
    v.coded_width = 2732;
    v.coded_height = 1536;

    // Unknown format: still conservative, and this resolution legitimately
    // exceeds the flat-4 fallback charge.
    assert!(p.validate(&budget).is_err());

    // Known, real format: charged its own (smaller) average bytes per
    // pixel, and now fits.
    let Some(v) = p.video.as_mut() else {
        panic!("CodecParameters::video() must carry video parameters");
    };
    v.format = Some(vaco_pixfmt::PixFmt::Yuv420p);
    assert!(
        p.validate(&budget).is_ok(),
        "a real yuv420p frame this size must fit `strict`'s frame budget"
    );
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
