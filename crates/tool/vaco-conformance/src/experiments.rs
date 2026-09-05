//! Format-calibration experiments exposed as a stable, reviewable catalogue.
//!
//! The container implementation needs a small set of behaviours the published
//! specifications intentionally leave open.  Each entry says exactly which
//! reference-only observation resolves that behaviour, so an implementation
//! agent can reproduce evidence without treating an old prose finding as an
//! oracle.  The recipes contain no reference output and never inspect source.

/// Whether an experiment interrogates a reference binary or reviews public
/// format documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExperimentKind {
    /// Generate or transform local media and observe `ffmpeg` or `ffprobe`.
    ReferenceOracle,
    /// Review public format documentation; no reference execution is involved.
    DocumentReview,
}

/// One stable calibration handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Experiment {
    /// Short identifier used in findings and implementation notes.
    pub id: &'static str,
    /// What a successful observation decides.
    pub question: &'static str,
    /// The category of evidence required for this row.
    pub kind: ExperimentKind,
    /// A self-contained black-box procedure or documentary review target.
    pub recipe: &'static str,
}

/// Return every format-calibration row in stable plan order.
///
/// There are twenty-six executable reference observations and one documentary
/// classification review.  Returning a fixed slice makes that distinction
/// inspectable without a second hand-maintained count.
#[must_use]
pub const fn catalogue() -> &'static [Experiment] {
    &CATALOGUE
}

/// Find one calibration row by its stable identifier.
///
/// Identifiers are deliberately case-sensitive: a report that says `P3` must
/// not silently select a different future namespace such as `p3`.
#[must_use]
pub fn by_id(id: &str) -> Option<&'static Experiment> {
    catalogue().iter().find(|experiment| experiment.id == id)
}

static CATALOGUE: [Experiment; 27] = [
    Experiment {
        id: "P1",
        question: "Does a MIME type rescue a zero content-probe score?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Serve mangled WebM bytes as .bin with Content-Type: video/webm; capture ffprobe's MIME probe-score diagnostic.",
    },
    Experiment {
        id: "P2",
        question: "What are the effective minimum and maximum probe-buffer sizes?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Move a container's only signature through a generated file and bisect the last offset ffprobe still identifies.",
    },
    Experiment {
        id: "P3",
        question: "Does forced demuxer selection report probe_score=100?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Generate Matroska locally; run ffprobe -f matroska -show_format and inspect probe_score.",
    },
    Experiment {
        id: "P4",
        question: "How many frames does the default frame-rate probe inspect?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Generate a stream whose cadence changes after frame N, then bisect N while reading r_frame_rate.",
    },
    Experiment {
        id: "P5",
        question: "Which rates snap to standard r_frame_rate values, and at what tolerance?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Generate millisecond-timebase Matroska at 23.976, 29.97, 59.94 and 119.88 fps; compare ffprobe r_frame_rate.",
    },
    Experiment {
        id: "P6",
        question: "What r_frame_rate is reported for one-frame container streams?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Generate one-frame MP4, Matroska and MPEG-TS files; compare ffprobe stream rate fields.",
    },
    Experiment {
        id: "P7",
        question: "What analyzeduration reaches a late second-program stream?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Generate MPEG-TS with a second program whose audio begins at t seconds; bisect t and inspect the reported streams.",
    },
    Experiment {
        id: "T1",
        question: "How does seek behave across a 33-bit PTS wrap?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Construct MPEG-TS segments straddling 2^33 ticks; seek past the wrap and compare ffprobe -show_packets output.",
    },
    Experiment {
        id: "T2",
        question: "Is container start_time the minimum or maximum stream start?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Build MP4 with audio at 0.000000 and video at 0.041708; inspect ffprobe format and stream start_time fields.",
    },
    Experiment {
        id: "T3",
        question: "Does MP4 duration follow mvhd or the longest track?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Patch a generated MP4 so mvhd says 10 seconds and its longest track says 12; inspect ffprobe duration.",
    },
    Experiment {
        id: "T4",
        question: "What tail data does the default duration probe consume?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Truncate an MPEG-TS tail progressively and record the first wrong ffprobe duration.",
    },
    Experiment {
        id: "T5",
        question: "What byte-level effect does each avoid_negative_ts mode have?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Remux a negative-DTS MP4 to MPEG-TS with all four avoid_negative_ts values and compare output bytes and packet timestamps.",
    },
    Experiment {
        id: "S1",
        question: "Does unseekable seeking forward-discard, and what bounds it?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Pipe a large MPEG-TS into ffprobe -ss 3600 -i - and measure packet output, bytes consumed and termination.",
    },
    Experiment {
        id: "M1",
        question: "What packet order does MP4 emit when chunks are all-video then all-audio?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Build a deliberately chunked MP4 and inspect ffprobe -show_packets stream-index order.",
    },
    Experiment {
        id: "M2",
        question: "How do ctts versions and cslg affect DTS shift?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Probe B-frame MP4s from three muxers covering ctts v0, ctts v1 and cslg; compare packet and format timestamps.",
    },
    Experiment {
        id: "M3",
        question: "What does an edit list with media_rate != 1 do?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Hand-build or patch an MP4 with a rate-2 elst entry, then inspect ffprobe packet timestamps and duration.",
    },
    Experiment {
        id: "M4",
        question: "Which chapter source wins when chpl and tref/chap disagree?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Build MP4 carrying both chapter forms with distinct titles; inspect ffprobe -show_chapters metadata.",
    },
    Experiment {
        id: "M5",
        question: "Is CENC IV generation deterministic under bitexact mode?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Mux the same input twice with encryption_scheme=cenc-aes-ctr and bitexact output flags; compare bytes and IV-related boxes.",
    },
    Experiment {
        id: "M6",
        question: "How does a track with avc1 and hvc1 stsd entries report its active codec?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Construct MP4 with avc1 and hvc1 sample entries in one track; inspect stream metadata and packets.",
    },
    Experiment {
        id: "M7",
        question: "Does MP4 nb_frames retain table count after mdat truncation?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Truncate a generated MP4 mdat to half; inspect ffprobe nb_frames and emitted packet count.",
    },
    Experiment {
        id: "K1",
        question: "How are EBML-laced frame timestamps derived without BlockDuration?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Generate Vorbis-in-Matroska with EBML lacing and no BlockDuration; inspect ffprobe packet PTS and duration sequence.",
    },
    Experiment {
        id: "K2",
        question: "How is Info/Duration=12345.6789 rounded?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Patch a Matroska Info Duration float to 12345.6789 and inspect ffprobe duration fields.",
    },
    Experiment {
        id: "K3",
        question: "What separator flattens nested SimpleTag values?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Generate Matroska with a two-level SimpleTag tree and inspect ffprobe tag keys and values.",
    },
    Experiment {
        id: "K4",
        question: "Which Matroska identifiers become deterministic under bitexact mode?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Mux identical input twice with output-side bitexact flags; diff SegmentUID, TrackUID and complete bytes.",
    },
    Experiment {
        id: "A1",
        question: "Do asf and asf_o differ observably?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Run both demuxer names over every locally generated ASF corpus member and diff ffprobe output.",
    },
    Experiment {
        id: "N1",
        question: "What breaks an equal-DTS interleave tie?",
        kind: ExperimentKind::ReferenceOracle,
        recipe: "Create two streams with identical DTS, remux to Matroska and MP4, then inspect ffprobe packet order.",
    },
    Experiment {
        id: "L1",
        question: "Are WavPack and TTA documented formats rather than unavailable black boxes?",
        kind: ExperimentKind::DocumentReview,
        recipe: "Review wavpack.com's format description and the public TTA specification; record the resulting implementation tier.",
    },
];
