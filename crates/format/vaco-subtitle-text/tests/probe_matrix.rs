//! Cross-format probe discipline (D17, `planning/AGENT-CONSTRAINTS.md`
//! "Detection and demuxing ask different questions").
//!
//! Every probe in this crate must answer "is this plausibly *my* format?"
//! strictly enough that a sample of any other format in this crate, and a
//! sample of ordinary prose, never outscores the sample's true owner. This is
//! the automated version of the check the brief asked to run by hand against
//! every pair — it walks every `(sample, probe)` combination the crate
//! registers rather than the dozen pairs each module's own unit tests happen
//! to cover.

#![allow(clippy::unwrap_used, reason = "test code")]

use vaco_format_core::probe::ProbeData;

type Probe = fn(&ProbeData<'_>) -> vaco_format_core::probe::ProbeScore;

/// One format's name, its probe function, and a representative sample.
fn samples() -> Vec<(&'static str, Probe, &'static [u8])> {
    vec![
        ("srt", vaco_subtitle_text::srt::probe, b"1\n00:00:01,000 --> 00:00:02,000\nHello world\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond line\n"),
        ("webvtt", vaco_subtitle_text::webvtt::probe, b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello world\n\n00:00:03.000 --> 00:00:04.000\nSecond line\n"),
        ("ass", vaco_subtitle_text::ass::probe, b"[Script Info]\nScriptType: v4.00+\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Hello world\nDialogue: 0,0:00:04.00,0:00:05.00,Default,,0,0,0,,Second\n"),
        ("scc", vaco_subtitle_text::scc::probe, b"Scenarist_SCC V1.0\n\n00:00:01:00\t9420 9420 942c 942c\n\n00:00:02:00\t9420 9420\n\n"),
        ("microdvd", vaco_subtitle_text::microdvd::probe, b"{0}{25}Hello world\n{25}{50}Second line\n"),
        ("jacosub", vaco_subtitle_text::jacosub::probe, b"0:00:01.00 0:00:03.00 Hello world\n0:00:04.00 0:00:05.00 Second line\n"),
        ("lrc", vaco_subtitle_text::lrc::probe, b"[00:01.00]Hello world\n[00:03.00]Second line\n[00:05.00]Third\n"),
        ("ttml", vaco_subtitle_text::ttml::probe, b"<?xml version=\"1.0\"?>\n<tt xmlns=\"http://www.w3.org/ns/ttml\"><body><div>\n<p begin=\"00:00:01.000\" end=\"00:00:02.000\">Hello world</p>\n</div></body></tt>\n"),
        ("subviewer", vaco_subtitle_text::subviewer::probe, b"[INFORMATION]\n\n00:00:01.250,00:00:03.000\nHello world\n\n00:00:04.000,00:00:05.000\nSecond line\n"),
        ("subviewer1", vaco_subtitle_text::subviewer1::probe, b"[00:00:01]\nHello world\n[00:00:03]\nSecond line\n[00:00:05]\n"),
        ("mpsub", vaco_subtitle_text::mpsub::probe, b"FORMAT=TIME\n1.0 2.0\nHello world\n\n1.0 2.0\nSecond line\n"),
        ("pjs", vaco_subtitle_text::pjs::probe, b"10,50,\"Hello world\"\n60,90,\"Second line\"\n"),
        ("realtext", vaco_subtitle_text::realtext::probe, b"<time begin=\"00:00:01\" end=\"00:00:02\"/>Hello world\n<time begin=\"00:00:03\" end=\"00:00:04\"/>Second line\n"),
        ("sami", vaco_subtitle_text::sami::probe, b"<SAMI><BODY>\n<SYNC Start=1000><P>Hello world\n<SYNC Start=3000><P>Second line\n<SYNC Start=5000><P>&nbsp;\n</BODY></SAMI>\n"),
        ("vplayer", vaco_subtitle_text::vplayer::probe, b"00:00:01:Hello world\n00:00:03:Second line\n00:00:05:Third\n"),
        ("mpl2", vaco_subtitle_text::mpl2::probe, b"[10][50]Hello world\n[60][90]Second line\n"),
        ("stl", vaco_subtitle_text::stl::probe, b"00:00:01:12,00:00:03:00,Hello world\n00:00:04:00,00:00:05:00,Second line\n"),
    ]
}

const PROSE: &[u8] = b"The quick brown fox jumps over the lazy dog. This is an ordinary\nparagraph of English prose with no timing information in it at all.\nIt spans several lines so that any line-oriented probe sees plenty of\nplausible-looking text to reject.\n";

#[test]
fn every_probe_rejects_plain_prose() {
    for (name, probe, _) in samples() {
        let score = probe(&ProbeData::new(PROSE));
        assert!(
            score.is_none(),
            "{name}'s probe scored {score:?} on plain prose, expected NONE"
        );
    }
}

#[test]
fn every_sample_is_recognised_by_its_own_probe() {
    for (name, probe, sample) in samples() {
        let score = probe(&ProbeData::new(sample));
        assert!(
            !score.is_none(),
            "{name}'s probe scored NONE on its own sample"
        );
    }
}

#[test]
fn no_probe_outscores_a_samples_true_owner() {
    let all = samples();
    for (owner, _, sample) in &all {
        let data = ProbeData::new(sample);
        let own_score = all
            .iter()
            .find(|(n, _, _)| n == owner)
            .map(|(_, p, _)| p(&data))
            .unwrap();
        for (other, probe, _) in &all {
            if other == owner {
                continue;
            }
            let foreign_score = probe(&data);
            assert!(
                foreign_score < own_score,
                "{other}'s probe scored {foreign_score:?} on {owner}'s sample, \
                 which is not less than {owner}'s own score {own_score:?} — \
                 a real file in this shape could be mis-detected"
            );
        }
    }
}
