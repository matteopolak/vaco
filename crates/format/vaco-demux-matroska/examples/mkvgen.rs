//! Write synthetic Matroska fixtures the reference muxer cannot produce.
//!
//! `cargo run -p vaco-demux-matroska --example mkvgen -- <outdir>`

use vaco_demux_matroska::ebml::schema as el;
use vaco_demux_matroska::synth::{self, SegmentSize};

fn frame(n: usize, fill: u8) -> Vec<u8> {
    vec![fill; n]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).ok_or("usage: mkvgen <outdir>")?;
    std::fs::create_dir_all(&dir)?;

    // A track that permits lacing, with a DefaultDuration of 20 ms.
    let audio_track = {
        let mut audio = synth::float(el::SAMPLINGFREQUENCY, 48000.0);
        audio.extend_from_slice(&synth::uint(el::CHANNELS, 2));
        let mut body = synth::uint(el::TRACKNUMBER, 1);
        body.extend_from_slice(&synth::uint(el::TRACKUID, 1));
        body.extend_from_slice(&synth::uint(el::TRACKTYPE, 2));
        body.extend_from_slice(&synth::string(el::CODECID, "A_PCM/INT/LIT"));
        body.extend_from_slice(&synth::uint(el::FLAGLACING, 1));
        body.extend_from_slice(&synth::uint(el::DEFAULTDURATION, 20_000_000));
        body.extend_from_slice(&synth::element(el::AUDIO, &audio));
        synth::element(el::TRACKENTRY, &body)
    };

    let f1 = frame(80, 0xA1);
    let f2 = frame(50, 0xB2);
    let f3 = frame(100, 0xC3);
    let frames: [&[u8]; 3] = [&f1, &f2, &f3];

    for (name, flags, payload) in [
        ("xiph", 0x02u8, synth::xiph_lace(&frames)),
        ("ebml", 0x06, synth::ebml_lace(&frames)),
        (
            "fixed",
            0x04,
            synth::fixed_lace(&[&f1, &frame(80, 0xB2), &frame(80, 0xC3)]),
        ),
        ("none", 0x00, f1.clone()),
    ] {
        let block = synth::element(
            el::SIMPLEBLOCK,
            &synth::block_body(1, 0, 0x80 | flags, &payload),
        );
        let cluster = synth::cluster(0, &[block], SegmentSize::Known);
        let info = synth::uint(el::TIMESTAMPSCALE, 1_000_000);
        let bytes = synth::file(
            "matroska",
            &info,
            &audio_track,
            &[cluster],
            SegmentSize::Known,
        );
        std::fs::write(format!("{dir}/lace_{name}.mkv"), &bytes)?;
    }

    // TimestampScale of 100 ns per tick: the case every implementation that
    // assumes milliseconds gets wrong.
    {
        let mut info = synth::uint(el::TIMESTAMPSCALE, 100);
        info.extend_from_slice(&synth::float(el::DURATION, 20_000_000.0));
        let block = synth::element(el::SIMPLEBLOCK, &synth::block_body(1, 10_000, 0x80, &f1));
        let cluster = synth::cluster(10_000_000, &[block], SegmentSize::Known);
        let bytes = synth::file(
            "matroska",
            &info,
            &synth::video_track(1, "V_VP8", 160, 120),
            &[cluster],
            SegmentSize::Known,
        );
        std::fs::write(format!("{dir}/scale100.mkv"), &bytes)?;
    }

    // Unknown-size Segment *and* unknown-size Clusters, terminated only by the
    // schema (RFC 8794 section 6.2).
    {
        let info = synth::uint(el::TIMESTAMPSCALE, 1_000_000);
        let mk = |ts: u64, fill: u8| {
            synth::cluster(
                ts,
                &[synth::element(
                    el::SIMPLEBLOCK,
                    &synth::block_body(1, 0, 0x80, &frame(64, fill)),
                )],
                SegmentSize::Unknown,
            )
        };
        let bytes = synth::file(
            "webm",
            &info,
            &synth::video_track(1, "V_VP8", 160, 120),
            &[mk(0, 1), mk(100, 2), mk(200, 3)],
            SegmentSize::Unknown,
        );
        std::fs::write(format!("{dir}/unknown_size.webm"), &bytes)?;
    }

    // Header stripping: ContentCompAlgo 3 with a two-octet prefix.
    {
        let mut comp = synth::uint(el::CONTENTCOMPALGO, 3);
        comp.extend_from_slice(&synth::element(el::CONTENTCOMPSETTINGS, &[0xDE, 0xAD]));
        let mut enc = synth::uint(el::CONTENTENCODINGORDER, 0);
        enc.extend_from_slice(&synth::uint(el::CONTENTENCODINGSCOPE, 1));
        enc.extend_from_slice(&synth::uint(el::CONTENTENCODINGTYPE, 0));
        enc.extend_from_slice(&synth::element(el::CONTENTCOMPRESSION, &comp));
        let encodings = synth::element(
            el::CONTENTENCODINGS,
            &synth::element(el::CONTENTENCODING, &enc),
        );
        // A TrackEntry with the encodings spliced in.
        let track = {
            let mut body = synth::uint(el::TRACKNUMBER, 1);
            body.extend_from_slice(&synth::uint(el::TRACKUID, 1));
            body.extend_from_slice(&synth::uint(el::TRACKTYPE, 1));
            body.extend_from_slice(&synth::string(el::CODECID, "V_VP8"));
            let mut video = synth::uint(el::PIXELWIDTH, 160);
            video.extend_from_slice(&synth::uint(el::PIXELHEIGHT, 120));
            body.extend_from_slice(&synth::element(el::VIDEO, &video));
            body.extend_from_slice(&encodings);
            synth::element(el::TRACKENTRY, &body)
        };
        let block = synth::element(
            el::SIMPLEBLOCK,
            &synth::block_body(1, 0, 0x80, &frame(16, 0x5A)),
        );
        let cluster = synth::cluster(0, &[block], SegmentSize::Known);
        let bytes = synth::file(
            "matroska",
            &synth::uint(el::TIMESTAMPSCALE, 1_000_000),
            &track,
            &[cluster],
            SegmentSize::Known,
        );
        std::fs::write(format!("{dir}/headerstrip.mkv"), &bytes)?;
    }
    println!("wrote fixtures to {dir}");
    Ok(())
}
