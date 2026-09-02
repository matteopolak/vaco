//! Which input streams reach an output file.
//!
//! # Why this is an algorithm and not a heuristic
//!
//! This is the single most-used behaviour in the tool: every command line that
//! does not say `-map` goes through it, and getting it approximately right
//! means quietly transcoding the wrong track. Plan 14 §6.2 states the rule as
//! "the stream with the greatest `width × height`", which is **wrong on its
//! own** — measured against ffmpeg 8.1, disposition beats size:
//!
//! ```text
//! 320x240 (default) vs 640x480            -> 320x240 wins
//! 320x240 (default) vs 3840x2160          -> 3840x2160 wins
//! ```
//!
//! Both cannot hold unless the flag is worth a finite number of pixels, so the
//! bonus was bracketed by bisection on the second stream's area against a fixed
//! `320x240` (= 76 800) first stream carrying `default`:
//!
//! | second stream | area | area − 76 800 | winner |
//! |---|---|---|---|
//! | 1920x1080 | 2 073 600 | 1 996 800 | first (default) |
//! | 2048x2048 | 4 194 304 | 4 117 504 | first (default) |
//! | 2538x2000 | 5 076 000 | **4 999 200** | first (default) |
//! | 2539x2000 | 5 078 000 | **5 001 200** | second |
//! | 2400x2400 | 5 760 000 | 5 683 200 | second |
//!
//! The cliff sits between 4 999 200 and 5 001 200, so
//! [`DEFAULT_DISPOSITION_BONUS`] is exactly 5 000 000 and the comparison is
//! strict — a tie leaves the earlier stream in place.
//!
//! # The measured rules
//!
//! * **video** — `score = area + 5 000 000 × default`, where `area` is
//!   `width × height` for an ordinary stream and **0** for one flagged
//!   `attached_pic`. An attached picture is therefore never chosen over a real
//!   video track (a 4000x4000 cover lost to a 64x64 track) but *is* chosen when
//!   it is the only video in the file (an mp3 with cover art selects the cover).
//! * **audio** — `score = channels + 5 000 000 × default`. Bit rate and sample
//!   rate do **not** participate: 32 kbit/s beat 256 kbit/s, and 8 kHz beat
//!   48 kHz, in both cases because they came first.
//! * **subtitle** — the first in order. Kind-matching against the output's
//!   default subtitle encoder (plan 14 §6.2 Rule 5) is unreachable in a build
//!   with no encoders and is not implemented; see `docs/app/vaco-cli.md`.
//! * **data and attachment** — never auto-selected.
//! * **ties** — the earlier `(file index, stream index)` wins, across files as
//!   well as within one: two inputs each with an identical 640x480 stream
//!   select input 0's.
//!
//! # `-map`
//!
//! Any `-map` at all turns automatic selection off entirely, for every type.
//! Maps are applied in command-line order; a `-map -SPEC` removes from what
//! earlier maps accumulated and never adds; the same stream mapped twice is a
//! fan-out, not an error. `-vn`/`-an`/`-sn`/`-dn` filter the result of *both*
//! paths — `-map 0 -vn` on a four-stream file yields the two audio streams.
//! All observed.
//!
//! # Two corners that took nine probes to pin down
//!
//! **An output whose maps all matched nothing is dropped, not an error.**
//! `ffmpeg -i in.mkv -map 0:v:9? -c copy -f null -` exits **0** and creates no
//! file at all, while `-vn -an -sn -dn` and `-map 0:v:0 -map -0:v:0` both exit
//! **234** with "Output file does not contain any stream". The discriminator is
//! not "did a stream reach the output" — `-map 0:v -vn` exits 234 with no
//! streams — it is **did any positive map match any input stream at all**. Nine
//! invocations, all consistent:
//!
//! | maps | a positive map matched? | `$?` |
//! |---|---|---|
//! | `0:v:9?` | no | 0 |
//! | `0:d?` | no | 0 |
//! | `-0:v?` `0:a:9?` | no | 0 |
//! | `0:v:9?` `0:a:0` `-0:a:0` | yes | 234 |
//! | `0:v:0` `-0:v:0` | yes | 234 |
//! | `0:v` with `-vn` | yes | 234 |
//! | `0` with `-vn -an -sn -dn` | yes | 234 |
//! | (none — auto selection) | n/a, no maps | 234 |
//!
//! [`Selection::dropped`] is that flag.
//!
//! **A negative map errors only when nothing has been accumulated yet.**
//! `-map -0:v` and `-map -0:v -map 0:a` both fail with "Stream map '' matches
//! no streams", while `-map 0:a -map -0:v` and `-map 0:v:0 -map -0:v:1`
//! succeed — and those two remove nothing either. The predicate that fits every
//! observation is *the accumulated list is empty*, not *this map removed
//! nothing*. `?` suppresses it. Whether the reference's real condition is
//! emptiness or something that coincides with it on these inputs is not
//! settled; the six probes are in the tests.

use std::collections::HashSet;

use vaco_cli_core::map::MapSpec;
use vaco_cli_core::{Disposition, MatchCtx, ProgramInfo, StreamInfo};
use vaco_core::MediaType;

use crate::complexgraph::ComplexPad;
use crate::exit::{AvError, Diagnostic};

/// What a `default` disposition is worth, in pixels for video and in channels
/// for audio. Bracketed to 5 000 000 by bisection; see the module docs.
pub const DEFAULT_DISPOSITION_BONUS: u64 = 5_000_000;

/// One input file, as stream selection sees it.
#[derive(Debug, Default, Clone)]
pub struct InputStreams {
    /// Container order. Both the specifier matcher and the scorer read this.
    pub streams: Vec<StreamInfo>,
    pub programs: Vec<ProgramInfo>,
    /// Parallel to [`InputStreams::streams`]: the channel count, which
    /// [`StreamInfo`] does not carry and audio auto-selection scores on.
    pub channels: Vec<u32>,
    /// Parallel to [`InputStreams::streams`]: the container's own display
    /// transformation matrix (`StreamSideData::DisplayMatrix`), for
    /// `-autorotate`'s default-on rotation. Not on [`StreamInfo`] itself for
    /// the same reason `channels` is not -- that type is the specifier
    /// grammar's view of a stream (see this module's own doc), and no
    /// specifier ever matches on a display matrix.
    pub display_matrix: Vec<Option<[i32; 9]>>,
}

impl InputStreams {
    /// Append a stream described by primitives.
    ///
    /// For tests and for the `cli_run` fuzz target, neither of which has a
    /// container to read. `kind` is `0` video, `1` audio, `2` subtitle, `3`
    /// data and anything else untyped; `disposition` is the 19-bit flag word
    /// [`vaco_cli_core::Disposition`] uses, of which only `DEFAULT` (bit 0) and
    /// `ATTACHED_PIC` (bit 10) affect selection.
    pub fn push_described(
        &mut self,
        kind: u8,
        width: u32,
        height: u32,
        channels: u32,
        disposition: u32,
    ) {
        let index = self.streams.len() as u32;
        self.streams.push(StreamInfo {
            index,
            media_type: match kind {
                0 => Some(MediaType::Video),
                1 => Some(MediaType::Audio),
                2 => Some(MediaType::Subtitle),
                3 => Some(MediaType::Data),
                _ => None,
            },
            disposition: Disposition::from_bits(disposition),
            codec_known: true,
            width,
            height,
            sample_rate: if kind == 1 { 48_000 } else { 0 },
            ..StreamInfo::default()
        });
        self.channels.push(channels);
        self.display_matrix.push(None);
    }

    fn ctx(&self) -> MatchCtx<'_> {
        MatchCtx {
            streams: &self.streams,
            programs: &self.programs,
            groups: &[],
        }
    }

    fn channels_of(&self, index: usize) -> u32 {
        self.channels.get(index).copied().unwrap_or(0)
    }
}

/// One selected stream: a real demuxed stream, or a `-filter_complex`/
/// `-lavfi` output pad resolved through `-map [label]`.
///
/// CL-25: this used to be a bare `(file, stream)` pair into real demuxer
/// streams, with no way to name anything else. `Complex` indexes into the
/// flat, labels-only catalog [`crate::complexgraph::catalog`] builds once for
/// the whole invocation — see that module's docs for how the same index
/// space is shared between `-map` resolution here and the real taps
/// `crate::exec::run_pipeline` attaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamPick {
    Demuxed { file: u32, stream: u32 },
    Complex(usize),
}

impl StreamPick {
    #[must_use]
    pub const fn demuxed(file: u32, stream: u32) -> Self {
        Self::Demuxed { file, stream }
    }

    /// The `(file, stream)` pair, for a real demuxed pick. `None` for a
    /// complex-graph pick, which has no such pair.
    #[must_use]
    pub const fn as_demuxed(&self) -> Option<(u32, u32)> {
        match self {
            Self::Demuxed { file, stream } => Some((*file, *stream)),
            Self::Complex(_) => None,
        }
    }
}

/// A `-map` occurrence, with the text the reference echoes back on failure.
#[derive(Debug, Clone)]
pub struct MapEntry {
    /// The value exactly as written, for `Failed to set value '…'`.
    pub text: String,
    pub spec: MapSpec,
}

impl MapEntry {
    /// Parse a `-map` value, keeping the text for the diagnostic.
    ///
    /// # Errors
    ///
    /// The reference's two-line shape: the specifier grammar's own complaint,
    /// then `Failed to set value '…' for option 'map'`.
    pub fn parse(text: &str) -> Result<Self, Diagnostic> {
        match MapSpec::parse(text) {
            Ok(spec) => Ok(Self {
                text: text.to_owned(),
                spec,
            }),
            Err(e) => Err(Diagnostic::new(
                AvError::EINVAL,
                vec![
                    e.to_string(),
                    format!(
                        "Failed to set value '{text}' for option 'map': {}",
                        AvError::EINVAL.text
                    ),
                ],
            )),
        }
    }
}

/// Media types suppressed by `-vn`/`-an`/`-sn`/`-dn` on this output.
///
/// Four independent flags rather than a state machine because that is what the
/// four options are: any subset of them can be given, and no combination means
/// anything other than the union.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one bool per option, and the options are independent"
)]
pub struct Suppressed {
    pub video: bool,
    pub audio: bool,
    pub subtitle: bool,
    pub data: bool,
}

impl Suppressed {
    #[must_use]
    pub const fn blocks(self, media: Option<MediaType>) -> bool {
        match media {
            Some(MediaType::Video) => self.video,
            Some(MediaType::Audio) => self.audio,
            Some(MediaType::Subtitle) => self.subtitle,
            Some(MediaType::Data) => self.data,
            _ => false,
        }
    }
}

fn score(s: &StreamInfo, channels: u32) -> u64 {
    let bonus = if s.disposition.contains(Disposition::DEFAULT) {
        DEFAULT_DISPOSITION_BONUS
    } else {
        0
    };
    let base = match s.media_type {
        Some(MediaType::Video) => {
            if s.disposition.contains(Disposition::ATTACHED_PIC) {
                0
            } else {
                u64::from(s.width) * u64::from(s.height)
            }
        }
        Some(MediaType::Audio) => u64::from(channels),
        _ => 0,
    };
    base.saturating_add(bonus)
}

/// The best stream of `media` across every input, or `None` when there is none.
///
/// Scans in `(file, stream)` order and keeps a strict `>`, so a tie leaves the
/// earlier stream in place — which is what the reference does across files as
/// well as within one.
#[must_use]
pub fn auto_pick(files: &[InputStreams], media: MediaType) -> Option<StreamPick> {
    let mut best: Option<(u64, StreamPick)> = None;
    for (fi, file) in files.iter().enumerate() {
        for (si, s) in file.streams.iter().enumerate() {
            if s.media_type != Some(media) {
                continue;
            }
            // Subtitles are picked by position, not by any property, so they
            // never enter the scoring comparison; the first one wins.
            if media == MediaType::Subtitle {
                return Some(StreamPick::demuxed(fi as u32, s.index));
            }
            let sc = score(s, file.channels_of(si));
            let pick = StreamPick::demuxed(fi as u32, s.index);
            if best.is_none_or(|(b, _)| sc > b) {
                best = Some((sc, pick));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// One output file's resolved stream list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub picks: Vec<StreamPick>,
    /// The output had `-map` entries and **no positive map matched any input
    /// stream**, so the reference drops the file silently and exits 0. See the
    /// module docs for the nine invocations that establish this.
    pub dropped: bool,
}

/// Resolve one output file's stream list.
///
/// `maps` empty runs automatic selection; any entry at all turns it off for
/// every type. `supports` is the output container's opinion, asked once per
/// media type. `complex` is the whole invocation's flat catalog of labelled
/// `-filter_complex`/`-lavfi` output pads (CL-25); `used_complex` accumulates
/// which of them have already been consumed by an earlier `-map [label]` —
/// threaded by the caller across every output file, since plan 14 §6.2 rule 4
/// says a labelled pad may be consumed **once**, not once per output.
///
/// # Errors
///
/// [`Diagnostic`] carrying the reference's wording and exit status: an
/// out-of-range input file index, a map that matched nothing and did not
/// carry `?`, or a `[label]` that names no open complex-graph output (or one
/// already used elsewhere).
#[allow(
    clippy::implicit_hasher,
    reason = "internal wiring type, not a public hashing surface"
)]
pub fn resolve(
    files: &[InputStreams],
    maps: &[MapEntry],
    blocked: Suppressed,
    supports: &dyn Fn(MediaType) -> bool,
    complex: &[ComplexPad],
    used_complex: &mut HashSet<usize>,
) -> Result<Selection, Diagnostic> {
    if maps.is_empty() {
        return Ok(Selection {
            picks: auto_select(files, blocked, supports),
            dropped: false,
        });
    }
    let mut out: Vec<StreamPick> = Vec::new();
    let mut matched = false;
    for m in maps {
        matched |= apply_map(files, m, blocked, &mut out, complex, used_complex)?;
    }
    Ok(Selection {
        picks: out,
        dropped: !matched,
    })
}

fn auto_select(
    files: &[InputStreams],
    blocked: Suppressed,
    supports: &dyn Fn(MediaType) -> bool,
) -> Vec<StreamPick> {
    let mut out = Vec::new();
    // Video, then audio, then subtitle: the order the reference emits them in,
    // which is also the order they appear in the output file.
    for media in [MediaType::Video, MediaType::Audio, MediaType::Subtitle] {
        if blocked.blocks(Some(media)) || !supports(media) {
            continue;
        }
        if let Some(p) = auto_pick(files, media) {
            out.push(p);
        }
    }
    out
}

/// Apply one map. Returns whether it was a **positive** map that matched at
/// least one input stream — the flag [`Selection::dropped`] is built from.
fn apply_map(
    files: &[InputStreams],
    m: &MapEntry,
    blocked: Suppressed,
    out: &mut Vec<StreamPick>,
    complex: &[ComplexPad],
    used_complex: &mut HashSet<usize>,
) -> Result<bool, Diagnostic> {
    let file_map = match &m.spec {
        MapSpec::Label(label) => {
            return apply_label_map(label, complex, used_complex, blocked, out);
        }
        MapSpec::File(f) => f,
    };

    let index = usize::try_from(file_map.file_index).ok();
    let Some(file) = index.and_then(|i| files.get(i)) else {
        // Observed: `-map -1:0` reports index **1**. The leading `-` is the
        // removal marker and is not part of the number.
        return Err(map_failure(
            m,
            vec![format!(
                "Invalid input file index: {}.",
                file_map.file_index
            )],
        ));
    };
    let file_index = index.unwrap_or(0) as u32;

    let hits = file_map.spec.select(&file.ctx());

    if file_map.negative {
        // Never adds. Fails only when nothing has been accumulated yet — see
        // the module docs; `-map 0:a -map -0:v` removes nothing and is fine,
        // `-map -0:v -map 0:a` is not.
        if out.is_empty() && !file_map.allow_unused {
            return Err(map_failure(
                m,
                vec![
                    "Stream map '' matches no streams.".to_owned(),
                    "To ignore this, add a trailing '?' to the map.".to_owned(),
                ],
            ));
        }
        // A complex-graph pick never matches a demuxed negative spec, so it
        // is always kept.
        out.retain(|p| {
            p.as_demuxed()
                .is_none_or(|(f, s)| !(f == file_index && hits.contains(&s)))
        });
        return Ok(false);
    }

    if hits.is_empty() && !file_map.allow_unused {
        // D17: the reference prints an **empty** stream map here. Observed for
        // `-map 0:9`, `-map 0:v:9` and `-map 0:s` alike — the text it means to
        // echo is never filled in. Reproduced rather than repaired.
        return Err(map_failure(
            m,
            vec![
                "Stream map '' matches no streams.".to_owned(),
                "To ignore this, add a trailing '?' to the map.".to_owned(),
            ],
        ));
    }

    // "Matched" is measured *before* `-vn`/`-an` filtering: `-map 0:v -vn`
    // exits 234 rather than dropping the output, so a blocked match still
    // counts as a match.
    let matched = !hits.is_empty();
    for stream in hits {
        let media = file
            .streams
            .iter()
            .find(|s| s.index == stream)
            .and_then(|s| s.media_type);
        if blocked.blocks(media) {
            continue;
        }
        out.push(StreamPick::demuxed(file_index, stream));
    }
    Ok(matched)
}

/// `-map [label]`: resolve against the flat, invocation-wide catalog of
/// labelled complex-graph output pads.
///
/// Both "the label does not exist" and "the label was already consumed by an
/// earlier `-map`" share one message — measured (`ffmpeg 8.1`,
/// `-map '[out]' -map '[out]'`): "Output with label 'out' does not exist in
/// any defined filter graph, or was already used elsewhere." — so a single
/// lookup that treats an already-used pad as not found reproduces both cases
/// without distinguishing them, exactly as the reference does not.
fn apply_label_map(
    label: &str,
    complex: &[ComplexPad],
    used_complex: &mut HashSet<usize>,
    blocked: Suppressed,
    out: &mut Vec<StreamPick>,
) -> Result<bool, Diagnostic> {
    let found = complex
        .iter()
        .enumerate()
        .find(|(i, p)| p.label == label && !used_complex.contains(i));
    let Some((index, pad)) = found else {
        return Err(Diagnostic::new(
            AvError::EINVAL,
            vec![format!(
                "[out#0] Output with label '{label}' does not exist in any defined filter graph, or was already used elsewhere."
            )],
        ));
    };
    used_complex.insert(index);
    // Consistent with a `-map file:spec` positive match (see `apply_map`
    // above): a type this output blocks still counts as matched, so `-vn`
    // does not turn a real match into a dropped-file exit.
    if !blocked.blocks(Some(pad.media)) {
        out.push(StreamPick::Complex(index));
    }
    Ok(true)
}

fn map_failure(m: &MapEntry, mut lines: Vec<String>) -> Diagnostic {
    lines.push(format!(
        "Failed to set value '{}' for option 'map': {}",
        m.text,
        AvError::EINVAL.text
    ));
    Diagnostic::new(AvError::EINVAL, lines)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    /// [`resolve`] with an empty complex-graph catalog — the shape every
    /// test predating CL-25 already assumed.
    fn resolve_simple(
        files: &[InputStreams],
        maps: &[MapEntry],
        blocked: Suppressed,
        supports: &dyn Fn(MediaType) -> bool,
    ) -> Result<Selection, Diagnostic> {
        resolve(files, maps, blocked, supports, &[], &mut HashSet::new())
    }

    fn video(index: u32, w: u32, h: u32, disp: Disposition) -> StreamInfo {
        StreamInfo {
            index,
            media_type: Some(MediaType::Video),
            width: w,
            height: h,
            codec_known: true,
            disposition: disp,
            ..StreamInfo::default()
        }
    }

    fn audio(index: u32, disp: Disposition) -> StreamInfo {
        StreamInfo {
            index,
            media_type: Some(MediaType::Audio),
            sample_rate: 48_000,
            codec_known: true,
            disposition: disp,
            ..StreamInfo::default()
        }
    }

    fn file(streams: Vec<StreamInfo>, channels: Vec<u32>) -> InputStreams {
        let display_matrix = vec![None; streams.len()];
        InputStreams {
            streams,
            programs: Vec::new(),
            channels,
            display_matrix,
        }
    }

    fn map(text: &str) -> MapEntry {
        MapEntry {
            text: text.to_owned(),
            spec: MapSpec::parse(text).unwrap(),
        }
    }

    const ALL: &dyn Fn(MediaType) -> bool = &|_| true;

    /// The exact file `ffmpeg -i multi.mkv -f null -` was run against.
    fn multi() -> Vec<InputStreams> {
        vec![file(
            vec![
                video(0, 320, 240, Disposition::DEFAULT),
                video(1, 640, 480, Disposition::NONE),
                audio(2, Disposition::DEFAULT),
                audio(3, Disposition::NONE),
            ],
            vec![0, 0, 2, 6],
        )]
    }

    #[test]
    fn default_disposition_beats_a_larger_video() {
        // OBSERVED: ffmpeg 8.1 selects #0:0 and #0:2 from this file.
        let sel = resolve_simple(&multi(), &[], Suppressed::default(), ALL).unwrap();
        assert!(!sel.dropped);
        assert_eq!(
            sel.picks,
            vec![StreamPick::demuxed(0, 0), StreamPick::demuxed(0, 2),]
        );
    }

    #[test]
    fn size_beats_default_once_it_is_worth_more_than_five_million_pixels() {
        // The two sides of the measured cliff. 2538x2000 = 5 076 000 and
        // 2539x2000 = 5 078 000; the first stream is 320x240 with `default`.
        for (w, h, want) in [(2538_u32, 2000_u32, 0_u32), (2539, 2000, 1)] {
            let f = vec![file(
                vec![
                    video(0, 320, 240, Disposition::DEFAULT),
                    video(1, w, h, Disposition::NONE),
                ],
                vec![0, 0],
            )];
            let sel = resolve_simple(&f, &[], Suppressed::default(), ALL).unwrap();
            assert_eq!(
                sel.picks.first().map(|p| p.as_demuxed().unwrap().1),
                Some(want),
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn an_attached_picture_loses_to_any_real_video_but_wins_alone() {
        let pic = video(1, 4000, 4000, Disposition::ATTACHED_PIC);
        let real = video(0, 64, 64, Disposition::DEFAULT);
        let both = vec![file(vec![real.clone(), pic.clone()], vec![0, 0])];
        assert_eq!(
            auto_pick(&both, MediaType::Video),
            Some(StreamPick::demuxed(0, 0))
        );

        // An mp3 with cover art: the picture is the only video, and is chosen.
        let alone = vec![file(vec![audio(0, Disposition::NONE), pic], vec![2, 0])];
        assert_eq!(
            auto_pick(&alone, MediaType::Video),
            Some(StreamPick::demuxed(0, 1))
        );
    }

    #[test]
    fn audio_scores_on_channels_only() {
        // 2ch first, 6ch second, neither default -> the 6ch one.
        let f = vec![file(
            vec![audio(0, Disposition::NONE), audio(1, Disposition::NONE)],
            vec![2, 6],
        )];
        assert_eq!(
            auto_pick(&f, MediaType::Audio),
            Some(StreamPick::demuxed(0, 1))
        );
        // 6ch first -> still the 6ch one, now by position as well.
        let f = vec![file(
            vec![audio(0, Disposition::NONE), audio(1, Disposition::NONE)],
            vec![6, 2],
        )];
        assert_eq!(
            auto_pick(&f, MediaType::Audio),
            Some(StreamPick::demuxed(0, 0))
        );
    }

    #[test]
    fn ties_go_to_the_earlier_file() {
        let one = || file(vec![video(0, 640, 480, Disposition::NONE)], vec![0]);
        let files = vec![one(), one()];
        assert_eq!(
            auto_pick(&files, MediaType::Video),
            Some(StreamPick::demuxed(0, 0))
        );
    }

    #[test]
    fn data_and_attachment_are_never_auto_selected() {
        let s = StreamInfo {
            index: 0,
            media_type: Some(MediaType::Data),
            codec_known: true,
            ..StreamInfo::default()
        };
        let files = vec![file(vec![s], vec![0])];
        assert!(
            resolve_simple(&files, &[], Suppressed::default(), ALL)
                .unwrap()
                .picks
                .is_empty()
        );
    }

    #[test]
    fn an_unsupported_type_is_skipped() {
        let only_audio = &|m: MediaType| m == MediaType::Audio;
        let sel = resolve_simple(&multi(), &[], Suppressed::default(), only_audio).unwrap();
        assert_eq!(sel.picks, vec![StreamPick::demuxed(0, 2)]);
    }

    #[test]
    fn map_order_is_output_order() {
        // OBSERVED: `-map 0:a -map 0:v` emits 2,3,0,1 in that order.
        let sel = resolve_simple(
            &multi(),
            &[map("0:a"), map("0:v")],
            Suppressed::default(),
            ALL,
        )
        .unwrap();
        assert_eq!(
            sel.picks
                .iter()
                .map(|p| p.as_demuxed().unwrap().1)
                .collect::<Vec<_>>(),
            vec![2, 3, 0, 1]
        );
    }

    #[test]
    fn a_negative_map_removes_and_a_later_positive_re_adds() {
        // OBSERVED: `-map 0 -map -0:a:1 -map 0:a:1` restores stream 3 last.
        let sel = resolve_simple(
            &multi(),
            &[map("0"), map("-0:a:1"), map("0:a:1")],
            Suppressed::default(),
            ALL,
        )
        .unwrap();
        assert_eq!(
            sel.picks
                .iter()
                .map(|p| p.as_demuxed().unwrap().1)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        // And on its own it just removes.
        let sel = resolve_simple(
            &multi(),
            &[map("0"), map("-0:a:0")],
            Suppressed::default(),
            ALL,
        )
        .unwrap();
        assert_eq!(
            sel.picks
                .iter()
                .map(|p| p.as_demuxed().unwrap().1)
                .collect::<Vec<_>>(),
            vec![0, 1, 3]
        );
    }

    #[test]
    fn the_same_stream_twice_is_a_fan_out() {
        let sel = resolve_simple(
            &multi(),
            &[map("0:v:0"), map("0:v:0")],
            Suppressed::default(),
            ALL,
        )
        .unwrap();
        assert_eq!(sel.picks.len(), 2);
    }

    #[test]
    fn blocked_types_filter_mapped_streams_too() {
        // OBSERVED: `-map 0 -vn` yields streams 2 and 3.
        let blocked = Suppressed {
            video: true,
            ..Suppressed::default()
        };
        let sel = resolve_simple(&multi(), &[map("0")], blocked, ALL).unwrap();
        assert!(!sel.dropped, "a match that -vn then filters still counts");
        assert_eq!(
            sel.picks
                .iter()
                .map(|p| p.as_demuxed().unwrap().1)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn a_map_matching_nothing_is_an_error_unless_optional() {
        let e = resolve_simple(&multi(), &[map("0:v:9")], Suppressed::default(), ALL).unwrap_err();
        assert_eq!(
            e.render(),
            "Stream map '' matches no streams.\n\
             To ignore this, add a trailing '?' to the map.\n\
             Failed to set value '0:v:9' for option 'map': Invalid argument\n"
        );
        assert_eq!(e.exit.code(), 234);
        let sel = resolve_simple(&multi(), &[map("0:v:9?")], Suppressed::default(), ALL).unwrap();
        assert!(sel.picks.is_empty());
        assert!(
            sel.dropped,
            "no positive map matched, so the output is dropped"
        );
    }

    #[test]
    fn a_negative_map_fails_only_when_nothing_has_been_accumulated() {
        // All six OBSERVED against ffmpeg 8.1 on the four-stream file.
        let err_cases: &[&[&str]] = &[&["-0:v"], &["-0:v", "0:a"], &["-0:v:0", "0:v"]];
        for case in err_cases {
            let maps: Vec<MapEntry> = case.iter().map(|t| map(t)).collect();
            let e = resolve_simple(&multi(), &maps, Suppressed::default(), ALL).unwrap_err();
            assert!(
                e.render()
                    .starts_with("Stream map '' matches no streams.\n"),
                "{case:?}: {}",
                e.render()
            );
            assert_eq!(e.exit.code(), 234, "{case:?}");
        }
        let ok_cases: &[&[&str]] = &[
            &["0:a", "-0:v"],
            &["0:v:0", "-0:v:1"],
            &["0:a:0", "-0:a:1"],
            &["-0:v?"],
            &["-0:v?", "0:a:9?"],
        ];
        for case in ok_cases {
            let maps: Vec<MapEntry> = case.iter().map(|t| map(t)).collect();
            assert!(
                resolve_simple(&multi(), &maps, Suppressed::default(), ALL).is_ok(),
                "{case:?}"
            );
        }
    }

    #[test]
    fn an_output_is_dropped_only_when_no_positive_map_matched() {
        // The nine-invocation table in the module docs, as code.
        let cases: &[(&[&str], bool)] = &[
            (&["0:v:9?"], true),
            (&["0:d?"], true),
            (&["-0:v?", "0:a:9?"], true),
            (&["0:v:9?", "0:a:0", "-0:a:0"], false),
            (&["0:v:0", "-0:v:0"], false),
            (&["0:v"], false),
            (&["0"], false),
        ];
        for (case, want_dropped) in cases {
            let maps: Vec<MapEntry> = case.iter().map(|t| map(t)).collect();
            let sel = resolve_simple(&multi(), &maps, Suppressed::default(), ALL).unwrap();
            assert_eq!(sel.dropped, *want_dropped, "{case:?}");
        }
        // No maps at all is never "dropped": auto-selection producing nothing
        // is an error, not a skipped file.
        let none = vec![];
        let blocked = Suppressed {
            video: true,
            audio: true,
            subtitle: true,
            data: true,
        };
        let sel = resolve_simple(&multi(), &none, blocked, ALL).unwrap();
        assert!(sel.picks.is_empty() && !sel.dropped);
    }

    #[test]
    fn an_out_of_range_file_index_names_the_index() {
        let e = resolve_simple(&multi(), &[map("9")], Suppressed::default(), ALL).unwrap_err();
        assert!(
            e.render().starts_with("Invalid input file index: 9.\n"),
            "{}",
            e.render()
        );
        assert_eq!(e.exit.code(), 234);
        // OBSERVED: `-map -1:0` reports index 1, not -1.
        let e = resolve_simple(&multi(), &[map("-1:0")], Suppressed::default(), ALL).unwrap_err();
        assert!(
            e.render().starts_with("Invalid input file index: 1.\n"),
            "{}",
            e.render()
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod properties {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::{
        DEFAULT_DISPOSITION_BONUS, InputStreams, MapEntry, StreamPick, Suppressed, auto_pick,
        resolve,
    };
    use vaco_cli_core::Disposition;
    use vaco_core::MediaType;

    /// `(kind, width, height, channels, disposition)`
    type RawStream = (u8, u32, u32, u32, u32);

    fn streams() -> impl Strategy<Value = Vec<Vec<RawStream>>> {
        prop::collection::vec(
            prop::collection::vec(
                (
                    0u8..6,
                    0u32..4096,
                    0u32..4096,
                    0u32..64,
                    prop::bits::u32::masked(
                        Disposition::DEFAULT.bits() | Disposition::ATTACHED_PIC.bits(),
                    ),
                ),
                0..6,
            ),
            0..3,
        )
    }

    fn build(raw: &[Vec<RawStream>]) -> Vec<InputStreams> {
        raw.iter()
            .map(|file| {
                let mut f = InputStreams::default();
                for &(k, w, h, c, d) in file {
                    f.push_described(k, w, h, c, d);
                }
                f
            })
            .collect()
    }

    /// The score the module documents, recomputed independently of the
    /// implementation so the property is a check and not a restatement.
    fn expected_score(kind: u8, w: u32, h: u32, ch: u32, disp: u32) -> u64 {
        let d = Disposition::from_bits(disp);
        let bonus = if d.contains(Disposition::DEFAULT) {
            DEFAULT_DISPOSITION_BONUS
        } else {
            0
        };
        let base = match kind {
            0 if d.contains(Disposition::ATTACHED_PIC) => 0,
            0 => u64::from(w) * u64::from(h),
            1 => u64::from(ch),
            _ => 0,
        };
        base + bonus
    }

    proptest! {
        /// Nothing beats the winner, and nothing earlier ties it — which is the
        /// tie-break rule stated as a property rather than as three examples.
        #[test]
        fn the_video_pick_is_the_first_maximum(raw in streams()) {
            let files = build(&raw);
            let Some(pick) = auto_pick(&files, MediaType::Video) else {
                prop_assert!(!raw.iter().any(|f| f.iter().any(|s| s.0 == 0)));
                return Ok(());
            };
            let (pf, ps) = pick.as_demuxed().unwrap();
            let winner = expected_score(
                raw[pf as usize][ps as usize].0,
                raw[pf as usize][ps as usize].1,
                raw[pf as usize][ps as usize].2,
                raw[pf as usize][ps as usize].3,
                raw[pf as usize][ps as usize].4,
            );
            let mut seen_winner = false;
            for (fi, file) in raw.iter().enumerate() {
                for (si, s) in file.iter().enumerate() {
                    if s.0 != 0 {
                        continue;
                    }
                    let here = StreamPick::demuxed(fi as u32, si as u32);
                    let score = expected_score(s.0, s.1, s.2, s.3, s.4);
                    if here == pick {
                        seen_winner = true;
                        continue;
                    }
                    if seen_winner {
                        prop_assert!(score <= winner, "a later stream outscored the pick");
                    } else {
                        prop_assert!(score < winner, "an earlier stream tied the pick");
                    }
                }
            }
            prop_assert!(seen_winner);
        }

        /// `-map <n>` is the identity on file `n`: every stream, in order, once.
        #[test]
        fn mapping_a_whole_file_selects_all_of_it_in_order(raw in streams()) {
            let files = build(&raw);
            for (fi, file) in raw.iter().enumerate() {
                let m = MapEntry::parse(&fi.to_string()).unwrap();
                let sel = resolve(&files, std::slice::from_ref(&m), Suppressed::default(), &|_| true, &[], &mut HashSet::new());
                if file.is_empty() {
                    // Nothing to match and no `?`, so it is the "matches no
                    // streams" error rather than a silent drop.
                    prop_assert!(sel.is_err());
                    continue;
                }
                let sel = sel.unwrap();
                prop_assert!(!sel.dropped);
                let want: Vec<u32> = (0..file.len() as u32).collect();
                let got: Vec<u32> = sel.picks.iter().map(|p| p.as_demuxed().unwrap().1).collect();
                prop_assert_eq!(got, want);
                prop_assert!(sel.picks.iter().all(|p| p.as_demuxed().unwrap().0 == fi as u32));
            }
        }

        /// A negative map is a left inverse of the positive one it mirrors, once
        /// something has been accumulated: `-map n -map -n` empties the list.
        #[test]
        fn a_negative_map_undoes_its_positive(raw in streams()) {
            let files = build(&raw);
            for (fi, file) in raw.iter().enumerate() {
                if file.is_empty() {
                    continue;
                }
                let maps = vec![
                    MapEntry::parse(&fi.to_string()).unwrap(),
                    MapEntry::parse(&format!("-{fi}")).unwrap(),
                ];
                let sel = resolve(&files, &maps, Suppressed::default(), &|_| true, &[], &mut HashSet::new()).unwrap();
                prop_assert!(sel.picks.is_empty());
                // A positive map *did* match, so this is an empty output rather
                // than a dropped one — the distinction the reference draws.
                prop_assert!(!sel.dropped);
            }
        }

        /// Automatic selection never picks more than one stream per media type,
        /// and never picks data or attachment.
        #[test]
        fn auto_selection_picks_at_most_one_of_each_type(raw in streams()) {
            let files = build(&raw);
            let sel = resolve(&files, &[], Suppressed::default(), &|_| true, &[], &mut HashSet::new()).unwrap();
            prop_assert!(sel.picks.len() <= 3);
            let kinds: Vec<u8> = sel
                .picks
                .iter()
                .map(|p| {
                    let (f, s) = p.as_demuxed().unwrap();
                    raw[f as usize][s as usize].0
                })
                .collect();
            let mut sorted = kinds.clone();
            sorted.sort_unstable();
            sorted.dedup();
            prop_assert_eq!(sorted.len(), kinds.len(), "two picks of one type");
            prop_assert!(kinds.iter().all(|k| *k < 3), "data or attachment was picked");
            // And video before audio before subtitle, which is output order.
            prop_assert!(kinds.windows(2).all(|w| w[0] < w[1]));
        }
    }
}
