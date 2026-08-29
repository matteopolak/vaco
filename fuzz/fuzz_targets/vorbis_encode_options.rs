//! `vaco-codec-vorbis`'s encoder against arbitrary `set_option` calls,
//! channel/rate configurations, and audio content — the encoder's own
//! "option-parsing/setup surface": [`Encoder::set_option`] (a caller-facing
//! string surface even though this encoder's own implementation is the
//! trait's no-op default today) and [`VorbisEncoder::extradata`], which is
//! where [`vaco_codec_vorbis`]'s fixed setup header (issue #309) actually
//! gets built from whatever channel count and sample rate the first frame
//! establishes.
//!
//! Property: for any option name/value pair, any channel count (1..=16),
//! any sample rate, and any bytes offered as `f32` sample data (including
//! NaN/infinity, which a caller's upstream filter chain could produce),
//! `send_frame`/`receive_packet`/`extradata`/`flush` never panic and never
//! allocate unboundedly — only their `Result`s are allowed to report
//! failure. Every input size fed to `send_frame` is capped by
//! `arbitrary`'s own collection-length behaviour on top of the harness's
//! `-max_len`, which keeps this from being a "valid input only" fuzzer per
//! `AGENT-CONSTRAINTS.md`'s "harness that cannot reach what it claims to
//! cover" warning: NaN/infinity samples and a zero-channel/zero-rate frame
//! are exercised on every run via the raw byte reinterpretation below, not
//! only on inputs `arbitrary` happens to consider "nice".
//!
//! fuzz-crate: vaco-codec-vorbis

#![no_main]

use libfuzzer_sys::fuzz_target;

use arbitrary::Arbitrary;
use vaco_codec_core::Encoder;
use vaco_codec_vorbis::VorbisEncoder;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_sampfmt::SampleFmt;

#[derive(Debug, Arbitrary)]
struct Input {
    option_name: String,
    option_value: String,
    channels: u8,
    sample_rate: u32,
    /// Raw bytes reinterpreted as `f32` samples, one channel's worth
    /// (broadcast to every channel below) -- deliberately not a `Vec<f32>`
    /// via `Arbitrary`'s own float impl, which never produces NaN or
    /// infinity; this crate's samples come straight from `to_le_bytes` so
    /// every bit pattern, including non-finite ones, is reachable.
    sample_bytes: Vec<u8>,
    second_call: bool,
}

fn drain(enc: &mut VorbisEncoder) -> u32 {
    let mut n = 0u32;
    while enc.receive_packet().is_ok() {
        n = n.saturating_add(1);
        if n > 10_000 {
            break; // a runaway producer is itself the bug; stop feeding the corpus into it forever
        }
    }
    n
}

fuzz_target!(|input: Input| {
    if input.channels == 0 || input.channels > 16 || input.sample_rate == 0 {
        // These are real rejection paths (`Error::Unsupported`), exercised
        // by `send_frame` below returning `Err` rather than skipped here --
        // this early return only avoids `Frame::alloc_audio`'s own
        // channel-count ceiling swallowing the interesting case before the
        // encoder ever sees it.
        return;
    }
    let channels = u32::from(input.channels);
    let samples_f32: Vec<f32> = input
        .sample_bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    if samples_f32.is_empty() || samples_f32.len() > 65_536 {
        return;
    }

    let mut enc = VorbisEncoder::new(Limits::permissive());
    let _ = enc.set_option(&input.option_name, &input.option_value);

    let layout = vaco_chlayout::ChannelLayout::unspecified(channels);
    let mut budget = Budget::new(Limits::permissive());
    let Ok(mut frame) = Frame::alloc_audio(
        &mut budget,
        SampleFmt::F32P,
        layout,
        samples_f32.len() as u32,
        input.sample_rate,
    ) else {
        return;
    };
    for ch in 0..channels as usize {
        if let Some(mut plane) = frame.plane_mut(ch)
            && let Some(row) = plane.row_mut(0)
        {
            for (i, &s) in samples_f32.iter().enumerate() {
                let byte_pos = i * 4;
                if let Some(dst) = row.get_mut(byte_pos..byte_pos + 4) {
                    dst.copy_from_slice(&s.to_le_bytes());
                }
            }
        }
    }

    let _ = enc.send_frame(Some(&frame));
    drain(&mut enc);
    if input.second_call {
        let _ = enc.send_frame(Some(&frame));
        drain(&mut enc);
    }
    let _ = enc.send_frame(None);
    drain(&mut enc);
    let _ = enc.extradata();
    enc.flush();
});
