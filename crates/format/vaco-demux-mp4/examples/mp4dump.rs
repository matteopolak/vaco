//! Print the fields `ffprobe` prints, so the two can be diffed directly.
//!
//! This exists because every rule in this crate is a measured one (plan 13
//! §1b), and a measurement nobody can re-take is a measurement nobody can
//! re-check. `cargo run -p vaco-demux-mp4 --example mp4dump -- file.mp4`
//! against `ffprobe -show_streams -show_format file.mp4`.

#![allow(clippy::integer_division, reason = "a bit rate is an integer")]

use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::{MediaSource, MemorySource};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: mp4dump <file> [packets]")?;
    let mode = args.next().unwrap_or_default();
    let packets = !mode.is_empty();
    let seek_to: Option<f64> = args.next().and_then(|v| v.parse().ok());
    let bytes = std::fs::read(&path)?;
    let size = bytes.len();
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    let mut demux = Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        Mp4Options::default(),
    )?;

    for s in demux.streams() {
        println!("[STREAM]");
        println!("index={}", s.index);
        println!(
            "codec_name={}",
            s.params
                .codec_id
                .map_or("N/A".into(), |c| c.name().to_owned())
        );
        println!(
            "codec_tag_string={}",
            s.params
                .codec_tag
                .map_or("N/A".to_owned(), |t| vaco_format_isom::FourCc(t)
                    .to_string())
        );
        println!("codec_type={:?}", s.media_type());
        if let Some(v) = &s.params.video {
            println!("width={}", v.width);
            println!("height={}", v.height);
            println!(
                "sample_aspect_ratio={}:{}",
                v.sample_aspect_ratio.num, v.sample_aspect_ratio.den
            );
        }
        if let Some(a) = &s.params.audio {
            println!("sample_rate={}", a.sample_rate);
        }
        println!("id=0x{:x}", s.id.unwrap_or(0));
        println!("r_frame_rate={}/{}", s.r_frame_rate.num, s.r_frame_rate.den);
        println!(
            "avg_frame_rate={}/{}",
            s.avg_frame_rate.num, s.avg_frame_rate.den
        );
        println!("time_base={}/{}", s.time_base.num, s.time_base.den);
        println!("start_pts={}", s.start_time);
        println!(
            "duration_ts={}",
            s.duration_ts.map_or("N/A".to_owned(), |v| v.to_string())
        );
        println!(
            "duration={}",
            s.duration()
                .map_or("N/A".to_owned(), |d| format!("{:.6}", d.as_secs_f64()))
        );
        println!(
            "bit_rate={}",
            s.params
                .bit_rate
                .map_or("N/A".to_owned(), |v| v.to_string())
        );
        println!(
            "nb_frames={}",
            s.frame_count.map_or("N/A".to_owned(), |v| v.to_string())
        );
        println!(
            "extradata_size={}",
            s.params.extradata.as_ref().map_or(0, Vec::len)
        );
        for (k, v) in s.disposition.fields() {
            println!("DISPOSITION:{k}={}", u8::from(v));
        }
        for (k, v) in &s.metadata {
            println!("TAG:{k}={v}");
        }
        println!("[/STREAM]");
    }
    println!("[FORMAT]");
    println!("nb_streams={}", demux.streams().len());
    println!("format_name={}", vaco_demux_mp4::FORMAT_NAME);
    println!(
        "start_time={:.6}",
        demux
            .streams()
            .iter()
            .filter(|s| !s.is_attached_pic())
            .filter_map(vaco_format_core::Stream::start_time_absolute)
            .map(vaco_core::Duration::as_secs_f64)
            .fold(f64::INFINITY, f64::min)
    );
    let dur = demux.duration();
    println!(
        "duration={}",
        dur.map_or("N/A".to_owned(), |d| format!("{:.6}", d.as_secs_f64()))
    );
    println!("size={size}");
    println!(
        "bit_rate={}",
        dur.filter(|d| d.as_micros() > 0)
            .map_or("N/A".to_owned(), |d| {
                ((size as i128 * 8 * 1_000_000) / i128::from(d.as_micros())).to_string()
            })
    );
    for (k, v) in demux.metadata() {
        println!("TAG:{k}={v}");
    }
    println!("[/FORMAT]");
    for c in demux.chapters() {
        println!("[CHAPTER] start={} title={:?}", c.start, c.metadata);
    }

    let tb: Vec<vaco_core::Rational> = demux.streams().iter().map(|s| s.time_base).collect();
    if let Some(seconds) = seek_to {
        let base = tb
            .first()
            .copied()
            .unwrap_or(vaco_core::Rational::new(1, 1000));
        let ticks = (seconds / base.to_f64()).round() as i64;
        demux.seek(
            vaco_format_core::SeekTarget::Timestamp {
                stream_index: 0,
                ts: vaco_core::Timestamp::new(ticks),
            },
            vaco_format_core::SeekFlags::BACKWARD,
        )?;
    }
    if packets {
        loop {
            match demux.read_packet() {
                Ok(p) => println!(
                    "packet|stream_index={}|pts={}|dts={}|duration={}|size={}|pos={}|flags={}{}{}",
                    p.stream_index,
                    p.pts,
                    p.dts,
                    tb.get(p.stream_index as usize)
                        .and_then(|b| p.duration.to_ticks(*b))
                        .unwrap_or(0),
                    p.payload().len(),
                    p.pos.unwrap_or(0),
                    if p.is_key() { "K" } else { "_" },
                    if p.flags.contains(vaco_packet::PacketFlags::DISCARD) {
                        "D"
                    } else {
                        "_"
                    },
                    if p.side_data.is_empty() { "" } else { " skip" },
                ),
                Err(vaco_core::Error::Eof) => break,
                Err(e) => {
                    println!("error: {e}");
                    break;
                }
            }
        }
    }
    Ok(())
}
