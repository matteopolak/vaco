//! The per-field tables: which fields, in which order, spelled how, and
//! **integer or string**.
//!
//! This is the module the whole crate exists to get right. `vaco-textformat`
//! decides how a field is *rendered*; nothing in it decides which fields there
//! are, what order they come in, or whether a given field goes through
//! [`TextFormat::int`](vaco_textformat::TextFormat::int) or
//! [`TextFormat::str`](vaco_textformat::TextFormat::str). There is no rule to
//! derive that from — `channels` is an integer and `sample_rate` is a string,
//! next to each other, both holding a plain number — so it is a table, and the
//! table was **measured**, never inferred.
//!
//! # Provenance
//!
//! Every row below was read off `ffprobe` 8.1 (Homebrew, arm64 macOS) under
//! `LC_ALL=C`, and each column has its own experiment:
//!
//! * **Order** — the order fields appear in `-of flat -show_optional_fields
//!   always`, which prints every field including the unavailable ones, so no
//!   field can hide.
//! * **Integer versus string** — cross-checked between two writers that spell
//!   the distinction differently, and which would have to be wrong in the same
//!   way to agree: `json` quotes strings and not integers, `flat` quotes
//!   strings and not integers. They agree on every row. Plan 13 §1b's rule is
//!   that the layer between you and the answer has opinions; two layers with
//!   different opinions agreeing is the cheapest way to buy confidence here.
//! * **The placeholder for an absent value** — `N/A` is *not* universal. The
//!   colour fields print `unknown`, `chroma_location` prints `unspecified`, and
//!   `level` prints the integer `-99`. Each was obtained by finding an input
//!   that genuinely lacks the value rather than by assuming.
//!
//! The commands, so the table can be re-derived when the pinned reference moves:
//!
//! ```sh
//! ffmpeg -f lavfi -i testsrc2=size=320x240:rate=25:duration=2 \
//!        -f lavfi -i sine=frequency=440:duration=2 \
//!        -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest av.mp4
//! ffmpeg -f lavfi -i testsrc2=size=32x24:rate=5:duration=0.4 -pix_fmt gray raw.yuv
//!
//! ffprobe -v quiet -of flat -show_optional_fields always -show_streams av.mp4
//! ffprobe -v quiet -of json                             -show_streams av.mp4
//! ffprobe -v quiet -of flat -show_optional_fields always \
//!         -f rawvideo -video_size 32x24 -pixel_format gray -show_streams raw.yuv
//! ```
//!
//! The third one is the interesting one: raw video has no aspect ratio, no
//! colour description, no level and no stream id, so it is the input that
//! reveals every placeholder at once.
//!
//! # How to change it
//!
//! Do not, without a reference run to back it. `tests/reference.rs` holds
//! captured `ffprobe` bytes; a change that does not move those bytes did not
//! change behaviour, and one that does move them needs the invocation recorded
//! beside the new bytes. `tests/fields.rs` additionally asserts that what the
//! emitters emit is exactly this table, in this order — so adding a field here
//! without emitting it, or emitting one that is not here, fails.

/// How a field reaches the writer.
///
/// The distinction between [`Ty::Int`] and everything else is the int-vs-string
/// table; the rest is which formatting helper the string goes through, all of
/// which live in `vaco_textformat::num` and none of which may be duplicated
/// here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    /// `TextFormat::int`. Unquoted in `json` and `flat`.
    Int,
    /// `TextFormat::str`, value already spelled.
    Str,
    /// `TextFormat::duration`: seconds, honouring `-sexagesimal` and `-unit`.
    Time,
    /// `TextFormat::value` with `Unit::Byte`: honours `-unit` and `-prefix`.
    Size,
    /// `TextFormat::value` with `Unit::BitPerSecond`.
    BitRate,
}

impl Ty {
    /// Whether the field is an integer field. **This is the table** the crate
    /// owed; everything else about a row is presentation.
    #[must_use]
    pub const fn is_int(self) -> bool {
        matches!(self, Self::Int)
    }
}

/// What a writer that prints unavailable optional fields prints for this one.
///
/// `json` and `xml` omit the field instead (they carry
/// `WriterFlags::SUPPRESS_OPTIONAL`); the choice between omitting and printing
/// the placeholder is `-show_optional_fields`, and it lives in
/// [`crate::emit`], not here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Absent {
    /// The field is always present. A `None` value would be a bug.
    Never,
    /// `N/A` — the common case.
    Na,
    /// A field-specific word. `color_range` and friends print `unknown`,
    /// `chroma_location` prints `unspecified`, and they are printed through the
    /// *optional* path, which is why `json` omits them entirely while `flat`
    /// shows them.
    Word(&'static str),
    /// The field is simply not emitted when the value is missing — no
    /// placeholder, in any writer, at any `-show_optional_fields` setting.
    /// `mime_codec_string` and `extradata_size` behave this way.
    Omit,
}

/// One field of one section.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Field {
    pub name: &'static str,
    pub ty: Ty,
    pub absent: Absent,
    /// Which streams the field applies to. [`Scope::Always`] for every other
    /// section.
    pub scope: Scope,
}

/// Which streams a `stream` field is emitted for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Always,
    Video,
    Audio,
    /// Video **and** subtitle, which is the scope `width`/`height` have: a
    /// subtitle stream prints them as `N/A` rather than omitting them.
    VideoOrSubtitle,
}

const fn f(name: &'static str, ty: Ty, absent: Absent) -> Field {
    Field {
        name,
        ty,
        absent,
        scope: Scope::Always,
    }
}

const fn v(name: &'static str, ty: Ty, absent: Absent) -> Field {
    Field {
        scope: Scope::Video,
        ..f(name, ty, absent)
    }
}

const fn a(name: &'static str, ty: Ty, absent: Absent) -> Field {
    Field {
        scope: Scope::Audio,
        ..f(name, ty, absent)
    }
}

use Absent::{Na, Never, Omit, Word};
use Ty::{BitRate, Int, Size, Str, Time};

/// The `stream` section, in emission order.
///
/// One flat table rather than a per-media-type one, because the *order* is the
/// thing being asserted and splitting it into three tables would let the three
/// drift apart. [`Scope`] does the filtering.
///
/// Two rows deserve a note:
///
/// * `sample_rate` is a **string** and `channels` is an integer, three rows
///   apart. So is `bits_per_raw_sample` (string) next to `extradata_size`
///   (integer). There is no derivable rule; this is why the table exists.
/// * `level` is the only field whose absent form is an *integer*: raw video
///   prints `level=-99`, not `level=N/A`. It is therefore [`Absent::Never`]
///   with the sentinel supplied by the emitter.
pub static STREAM: &[Field] = &[
    f("index", Int, Never),
    f("codec_name", Str, Word("unknown")),
    f("codec_long_name", Str, Word("unknown")),
    f("profile", Str, Word("unknown")),
    f("codec_type", Str, Word("unknown")),
    f("codec_tag_string", Str, Never),
    f("codec_tag", Str, Never),
    // Absent outright for a codec with no MIME mapping, and for every subtitle
    // stream. Not a placeholder — the field is simply not there.
    f("mime_codec_string", Str, Omit),
    // `N/A` for a subtitle stream, which is why this is not `Scope::Video`.
    Field {
        scope: Scope::VideoOrSubtitle,
        ..f("width", Int, Na)
    },
    Field {
        scope: Scope::VideoOrSubtitle,
        ..f("height", Int, Na)
    },
    v("coded_width", Int, Never),
    v("coded_height", Int, Never),
    v("has_b_frames", Int, Never),
    v("sample_aspect_ratio", Str, Na),
    v("display_aspect_ratio", Str, Na),
    v("pix_fmt", Str, Word("unknown")),
    // `-99` when unknown, as an integer. See the note above.
    v("level", Int, Never),
    v("color_range", Str, Word("unknown")),
    v("color_space", Str, Word("unknown")),
    v("color_transfer", Str, Word("unknown")),
    v("color_primaries", Str, Word("unknown")),
    // The one that spells its unknown differently. Observed, not a typo.
    v("chroma_location", Str, Word("unspecified")),
    v("field_order", Str, Word("unknown")),
    // Decoder **private** options (`-show_private_data`, on by default), which
    // is why the block between `field_order` and `id` changes shape per codec.
    // Measured, one file each:
    //
    //   h264 -> is_avc="true" nal_length_size="4"
    //   hevc -> view_ids_available="" view_pos_available=""
    //   av1  -> nothing at all
    //
    // `Omit`, so a codec that has none emits none — the reference does not
    // print a placeholder for a private option that does not exist.
    v("is_avc", Str, Omit),
    v("nal_length_size", Str, Omit),
    v("view_ids_available", Str, Omit),
    v("view_pos_available", Str, Omit),
    a("sample_fmt", Str, Word("unknown")),
    a("sample_rate", Str, Never),
    a("channels", Int, Never),
    a("channel_layout", Str, Word("unknown")),
    a("bits_per_sample", Int, Never),
    a("initial_padding", Int, Never),
    f("id", Str, Na),
    f("r_frame_rate", Str, Never),
    f("avg_frame_rate", Str, Never),
    f("time_base", Str, Never),
    f("start_pts", Int, Na),
    f("start_time", Time, Na),
    f("duration_ts", Int, Na),
    f("duration", Time, Na),
    f("bit_rate", BitRate, Na),
    f("max_bit_rate", BitRate, Na),
    f("bits_per_raw_sample", Str, Na),
    f("nb_frames", Str, Na),
    f("nb_read_frames", Str, Na),
    f("nb_read_packets", Str, Na),
    f("extradata_size", Int, Omit),
];

/// The `format` section, in emission order.
///
/// `nb_streams`, `nb_programs`, `nb_stream_groups` and `probe_score` are
/// integers; `size` and `bit_rate` are strings even though both hold a plain
/// number. Same table, same reason.
pub static FORMAT: &[Field] = &[
    f("filename", Str, Never),
    f("nb_streams", Int, Never),
    f("nb_programs", Int, Never),
    f("nb_stream_groups", Int, Never),
    f("format_name", Str, Never),
    f("format_long_name", Str, Word("unknown")),
    f("start_time", Time, Na),
    f("duration", Time, Na),
    f("size", Size, Na),
    f("bit_rate", BitRate, Na),
    f("probe_score", Int, Never),
];

/// The `packet` section, in emission order.
///
/// `stream_index` is an integer and `size` and `pos` are strings — again next
/// to each other, again holding plain numbers.
///
/// Measured with
///
/// ```sh
/// ffprobe -v quiet -of flat -show_optional_fields always -show_packets \
///         -read_intervals '%+#3' av.mp4
/// ```
///
/// across MP4, Matroska and MPEG-TS: the same eleven fields in the same order
/// in all three, so nothing here is container-specific.
///
/// Two rows are conditional rather than optional. `data` and `data_hash` exist
/// only when `-show_data` / `-show_data_hash` asked for them — `Absent::Omit`,
/// so `-show_optional_fields always` does **not** conjure an `N/A` for either,
/// which is what the run above confirms. `data` precedes `data_hash` when both
/// are on.
///
/// `duration` and `duration_time` are the two fields whose absent form is
/// **zero rather than a sentinel**: the reference prints `N/A` for a duration
/// of 0, which is not how `pts` behaves three rows above.
pub static PACKET: &[Field] = &[
    f("codec_type", Str, Word("unknown")),
    f("stream_index", Int, Never),
    f("pts", Int, Na),
    f("pts_time", Time, Na),
    f("dts", Int, Na),
    f("dts_time", Time, Na),
    f("duration", Int, Na),
    f("duration_time", Time, Na),
    f("size", Size, Na),
    // A plain integer string, **not** a `Size`. Measured, and it is exactly
    // the kind of row this table exists for:
    //
    //   -unit -prefix   ->   size=5.171000 Kbyte   pos=48
    //
    // `size` scales and takes a unit; `pos` one column later does neither,
    // under any of `-unit`, `-prefix`, `-byte_binary_prefix` or `-pretty`.
    // Typing it `Size` because it holds a byte count looks obviously right
    // and is wrong in four of the seven formatting modes.
    f("pos", Str, Na),
    f("flags", Str, Never),
    f("data", Str, Omit),
    f("data_hash", Str, Omit),
];

/// The `error` section.
pub static ERROR: &[Field] = &[f("code", Int, Never), f("string", Str, Never)];

/// The `program_version` section.
///
/// Never byte-compared against the reference: plan 13 §1.3.2's `strip-sections`
/// normaliser removes it, because these fields identify the producing software
/// and reproducing `FFmpeg`'s version prose would be both impossible and wrong.
/// The *shape* is reproduced; the values are Vaco's.
pub static PROGRAM_VERSION: &[Field] = &[
    f("version", Str, Never),
    f("copyright", Str, Never),
    f("compiler_ident", Str, Never),
    f("configuration", Str, Never),
];

/// One `library_version` element. Same normalisation note as above.
pub static LIBRARY_VERSION: &[Field] = &[
    f("name", Str, Never),
    f("major", Int, Never),
    f("minor", Int, Never),
    f("micro", Int, Never),
    f("version", Int, Never),
    f("ident", Str, Never),
];

/// Look a field up in a table.
///
/// Returns `None` for a name the table does not have, which the emitters treat
/// as "emit nothing" — a missing row can then never become a panic or a
/// mis-typed field.
#[must_use]
pub fn find(table: &'static [Field], name: &str) -> Option<&'static Field> {
    table.iter().find(|f| f.name == name)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    fn all_tables() -> Vec<(&'static str, &'static [Field])> {
        vec![
            ("stream", STREAM),
            ("format", FORMAT),
            ("packet", PACKET),
            ("error", ERROR),
            ("program_version", PROGRAM_VERSION),
            ("library_version", LIBRARY_VERSION),
        ]
    }

    #[test]
    fn field_names_are_unique_within_a_table() {
        for (name, table) in all_tables() {
            for (i, a) in table.iter().enumerate() {
                for b in table.iter().skip(i + 1) {
                    assert_ne!(a.name, b.name, "{name}: {} twice", a.name);
                }
            }
        }
    }

    #[test]
    fn scope_is_only_used_by_the_stream_table() {
        for (name, table) in all_tables() {
            if name == "stream" {
                continue;
            }
            assert!(
                table.iter().all(|f| f.scope == Scope::Always),
                "{name} uses Scope"
            );
        }
    }

    #[test]
    fn a_never_absent_field_is_not_given_a_placeholder() {
        // `Absent::Never` and a placeholder are contradictory; the emitter would
        // silently prefer one.
        for (name, table) in all_tables() {
            for field in table {
                if field.absent == Absent::Never {
                    continue;
                }
                assert!(
                    matches!(field.absent, Na | Word(_) | Omit),
                    "{name}.{}",
                    field.name
                );
            }
        }
    }

    #[test]
    fn the_measured_int_fields_are_exactly_these() {
        // A change to this list is a change to observable output. Anyone editing
        // it needs an `ffprobe -of json` run in the commit message.
        let ints: Vec<&str> = STREAM
            .iter()
            .filter(|f| f.ty.is_int())
            .map(|f| f.name)
            .collect();
        assert_eq!(
            ints,
            [
                "index",
                "width",
                "height",
                "coded_width",
                "coded_height",
                "has_b_frames",
                "level",
                "channels",
                "bits_per_sample",
                "initial_padding",
                "start_pts",
                "duration_ts",
                "extradata_size",
            ]
        );
        let format_ints: Vec<&str> = FORMAT
            .iter()
            .filter(|f| f.ty.is_int())
            .map(|f| f.name)
            .collect();
        assert_eq!(
            format_ints,
            [
                "nb_streams",
                "nb_programs",
                "nb_stream_groups",
                "probe_score"
            ]
        );
        let packet_ints: Vec<&str> = PACKET
            .iter()
            .filter(|f| f.ty.is_int())
            .map(|f| f.name)
            .collect();
        assert_eq!(packet_ints, ["stream_index", "pts", "dts", "duration"]);
    }

    #[test]
    fn the_number_shaped_string_fields_are_still_strings() {
        // The trap this crate exists to avoid: these all hold a plain number and
        // are all printed quoted.
        for name in [
            "sample_rate",
            "bits_per_raw_sample",
            "nb_frames",
            "nb_read_frames",
            "nb_read_packets",
            "id",
        ] {
            let field = find(STREAM, name).expect("in the table");
            assert!(!field.ty.is_int(), "stream.{name} must be a string");
        }
        for name in ["size", "bit_rate"] {
            let field = find(FORMAT, name).expect("in the table");
            assert!(!field.ty.is_int(), "format.{name} must be a string");
        }
        for name in ["size", "pos"] {
            let field = find(PACKET, name).expect("in the table");
            assert!(!field.ty.is_int(), "packet.{name} must be a string");
        }
    }

    #[test]
    fn lookup_misses_are_not_a_panic() {
        assert!(find(STREAM, "nonesuch").is_none());
        assert!(find(FORMAT, "index").is_none());
    }
}
