//! Format detection over attacker-chosen bytes.
//!
//! Probing is the first thing that touches an untrusted file and it runs before
//! anything has been validated, so every accessor on `ProbeData` has to be
//! total: the zero-padding window, the typed readers, the extension parser and
//! the scoring engine itself. A panic here is reachable from `vaco -i <file>`
//! with no options at all.
//! fuzz-crate: vaco-format-core

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_core::probe::{Probe, ProbeData};
use vaco_format_core::vacoraw;
use vaco_format_core::{DemuxerDesc, FormatOptions};

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    buf: Vec<u8>,
    filename: Option<String>,
    mime: Option<String>,
    whitelist: Option<String>,
    formatprobesize: i32,
}

fuzz_target!(|input: Input| {
    let mut opts = FormatOptions::default();
    opts.formatprobesize = input.formatprobesize;
    if let Some(w) = &input.whitelist {
        opts.format_whitelist = w.clone();
    }

    let cands: &[&DemuxerDesc] = &[&vacoraw::DEMUXER];
    let probe = Probe::new(cands, &opts);

    let mut data = ProbeData::new(&input.buf);
    if let Some(f) = &input.filename {
        data = data.with_filename(f);
    }
    if let Some(m) = &input.mime {
        data = data.with_mime_type(m);
    }

    // The winner, if any, must be inside the documented score space and must
    // never be a zero score (R5).
    if let Some(found) = probe.best(&data) {
        assert!(found.score.value() > 0, "a zero score won");
        assert!(found.score.value() <= 100, "score out of range");
        assert!(
            opts.format_allowed(found.desc.name),
            "the whitelist was not applied before probing"
        );
    }

    // `score_all` must be sorted and total.
    let all = probe.score_all(&data);
    for pair in all.windows(2) {
        assert!(pair[0].score >= pair[1].score, "score_all is not sorted");
    }

    // Every typed accessor, across the real bytes, the padding, and past it.
    let end = data.len().saturating_add(ProbeData::PADDING + 8).min(4096);
    for i in 0..end {
        let _ = data.get(i);
        let _ = data.rb16(i);
        let _ = data.rl16(i);
        let _ = data.rb32(i);
        let _ = data.rl32(i);
        let _ = data.rb64(i);
        let _ = data.rl64(i);
        let _ = data.tag(i);
        let _ = data.matches_at(i, b"VACORAW");
    }
    let _ = data.find(b"VACORAW", 0, usize::MAX);
    let _ = data.find(&[], 0, usize::MAX);
    let _ = data.extension();
    let _ = data.extension_matches(&["vacoraw", "mp4"]);

    // Forcing a format must never panic, whatever the name.
    if let Some(name) = &input.filename {
        let _ = probe.force(name);
    }
    let _ = probe.force("vacoraw");
});
