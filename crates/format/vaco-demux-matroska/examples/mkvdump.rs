//! Print what the demuxer sees, in a shape that lines up with `ffprobe`.
//!
//! `cargo run -p vaco-demux-matroska --example mkvdump -- clip.mkv [--packets]`

use vaco_demux_matroska::MatroskaDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::{MediaSource, MemorySource};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: mkvdump <file> [--packets]")?;
    let mode = args.next();
    let packets = mode.as_deref() == Some("--packets");
    let discover = mode.as_deref() == Some("--discover");
    let bytes = std::fs::read(&path)?;
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    let mut d = MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default())?;
    if discover {
        // The composition `vaco-probe` does NOT currently perform. Run it here
        // so the two halves of `start_time` can be checked against `ffprobe`.
        use vaco_format_core::discovery::Discovery;
        let mut disc = Discovery::new(d, vaco_demux_matroska::FLAGS, &FormatOptions::default());
        disc.run(&NoParsers)?;
        println!("stop_reason={}", disc.report().stop_reason.name());
        println!(
            "format start_time={:?}",
            disc.report().start_time.map(vaco_core::Duration::as_micros)
        );
        for s in disc.streams() {
            println!(
                "stream {} start_pts={:?} start_time={:?} initial_padding={:?}",
                s.index,
                s.start_time.ticks(),
                s.start_time_absolute().map(vaco_core::Duration::as_micros),
                s.params.audio.as_ref().map(|a| a.initial_padding)
            );
        }
        return Ok(());
    }
    println!("doc_type={} scale={}", d.doc_type(), d.timestamp_scale());
    println!("duration={:?}", d.duration());
    println!("metadata={:?}", d.metadata());
    for s in d.streams() {
        println!(
            "stream {} id={:?} media={:?} codec={:?} tb={} disp={:?} meta={:?}",
            s.index,
            s.id,
            s.media_type(),
            s.params.codec_id,
            s.time_base,
            s.disposition,
            s.metadata
        );
        if let Some(v) = &s.params.video {
            println!(
                "   video {}x{} coded {}x{} sar={} fps={} field={:?} color={:?}",
                v.width,
                v.height,
                v.coded_width,
                v.coded_height,
                v.sample_aspect_ratio,
                v.frame_rate,
                v.field_order,
                v.color
            );
        }
        if let Some(a) = &s.params.audio {
            println!(
                "   audio {} Hz layout={:?} bits={:?} initial_padding={}",
                a.sample_rate,
                a.layout
                    .as_ref()
                    .map(vaco_chlayout::ChannelLayout::describe),
                a.bits_per_raw_sample,
                a.initial_padding
            );
        }
        println!(
            "   extradata={:?}",
            s.params.extradata.as_ref().map(Vec::len)
        );
    }
    for c in d.chapters() {
        println!(
            "chapter {} {}..{} tb={} {:?}",
            c.id,
            c.start.ticks().unwrap_or(0),
            c.end.ticks().unwrap_or(0),
            c.time_base,
            c.metadata
        );
    }
    if packets {
        let mut n = 0;
        loop {
            match d.read_packet() {
                Ok(p) => {
                    println!(
                        "packet {} pts={:?} dur={} size={} pos={:?} flags={:?} side={:?}",
                        p.stream_index,
                        p.pts.ticks(),
                        p.duration.as_micros(),
                        p.len,
                        p.pos,
                        p.flags,
                        p.side_data
                    );
                    n += 1;
                    if n > 100_000 {
                        break;
                    }
                }
                Err(e) => {
                    println!("end: {e}");
                    break;
                }
            }
        }
        println!("{n} packets");
    }
    Ok(())
}
