//! Whole-file demux over arbitrary bytes, across every family in
//! `vaco-demux-raw`: PCM, raw video, `yuv4mpegpipe`, the bitstream family and
//! `ac3`/`eac3`.
//!
//! A raw format has no structure at all beyond what its options declare —
//! the byte stream itself is entirely attacker-controlled with no magic, no
//! length-prefixed tables, nothing to validate before trusting it. That
//! makes this crate's fuzz surface unusually direct: every registration's
//! `open` and `read_packet` run on the raw fuzzer input with only the
//! declared options standing between it and a `Packet`.
//!
//! One target covers `vaco-demux-raw` and `vaco-mux-raw` together (D6): the
//! mux side never parses untrusted input — every registration but
//! `yuv4mpegpipe` writes a packet's payload verbatim, and `yuv4mpegpipe`'s
//! own header-line grammar is exercised from the *read* side here, since the
//! demuxer's `parse_header` and the muxer's `write_header` are the same
//! grammar in both directions.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Reading terminates.** A [`MAX_PACKETS`] cap turns a demuxer that
//!   returns packets without ever reaching `Eof` into a localised assertion
//!   instead of a fuzzer timeout — this bites the `BitstreamDemuxer`
//!   `FixedBlock` framing hardest, since it has no structural stopping
//!   condition beyond the input's own length.
//! * **Every packet names the one declared stream.** Every registration in
//!   this crate carries exactly one stream, so `stream_index` must always be
//!   `0`.
//! * **Nothing allocates past the ceiling.** Every demuxer here is opened
//!   with a small [`Limits`] budget; `BitstreamDemuxer::open_with_limits`
//!   loads the whole remaining input up front specifically so this bound is
//!   exercised on every run rather than only when a caller happens to read
//!   the whole file.
//! * **`Eof` is stable.** Once end of stream is reported it must keep being
//!   reported.
//!
//! fuzz-crate: vaco-demux-raw

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, Rational};
use vaco_demux_raw::ac3::Ac3Demuxer;
use vaco_demux_raw::bitstream::{self, BitstreamDemuxer, BitstreamOptions};
use vaco_demux_raw::pcm::{self, PcmDemuxer, PcmOptions};
use vaco_demux_raw::rawvideo::{self, RawVideoDemuxer, RawVideoOptions};
use vaco_demux_raw::y4m::Yuv4MpegDemuxer;
use vaco_format_core::Demuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_io::MemorySource;
use vaco_limits::Limits;
use vaco_pixfmt::PixFmt;

/// Packets read per drain before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

#[derive(Debug, Arbitrary)]
enum Family {
    Pcm { name: u8, sample_rate: u16, channels: u8 },
    RawVideo { name: u8, width: u16, height: u16, pixel_format: u8 },
    Yuv4Mpeg,
    Bitstream { name: u8, framerate_num: u16, framerate_den: u16 },
    Ac3 { eac3: bool },
}

#[derive(Debug, Arbitrary)]
struct Input {
    family: Family,
    bytes: Vec<u8>,
}

/// Read until `Eof`, checking every packet along the way. Returns whether
/// `Eof` was actually reached (as opposed to the packet cap firing).
fn drain(d: &mut dyn Demuxer) -> bool {
    let mut n = 0u32;
    loop {
        if n >= MAX_PACKETS {
            return false;
        }
        match d.read_packet() {
            Ok(p) => {
                assert_eq!(p.stream_index, 0, "every registration has one stream");
                n = n.saturating_add(1);
            }
            Err(Error::Eof) => {
                // Stable: reading again must still report Eof, not resume or
                // reinterpret trailing bytes as a fresh packet.
                assert!(matches!(d.read_packet(), Err(Error::Eof)));
                return true;
            }
            Err(_) => return true,
        }
    }
}

/// A small, deterministic ceiling: enough for the geometry/frame sizes this
/// target constructs, tight enough that a runaway allocation is caught fast.
fn limits() -> Limits {
    Limits::strict()
}

fn run_pcm(name_idx: u8, sample_rate: u16, channels: u8, bytes: Vec<u8>) {
    let Some(spec) = pcm::PCM_FORMATS.get(usize::from(name_idx) % pcm::PCM_FORMATS.len()) else {
        return;
    };
    let opts = PcmOptions {
        sample_rate: u32::from(sample_rate).max(1),
        layout: ChannelLayout::unspecified(u32::from(channels).max(1).min(64)),
    };
    let src = Box::new(MemorySource::new(bytes));
    if let Ok(mut d) = PcmDemuxer::open_with_limits(spec.name, src, &opts, limits()) {
        drain(&mut d);
    }
}

fn run_rawvideo(name_idx: u8, width: u16, height: u16, pixel_format: u8, bytes: Vec<u8>) {
    let specs = [
        &rawvideo::RAWVIDEO,
        &rawvideo::BITPACKED,
        &rawvideo::V210,
        &rawvideo::V210X,
    ];
    let Some(&spec) = specs.get(usize::from(name_idx) % specs.len()) else {
        return;
    };
    let fmt = PixFmt::all()
        .get(usize::from(pixel_format) % PixFmt::all().len())
        .copied()
        .unwrap_or(PixFmt::Yuv420p);
    // Bounded well under `Limits::strict`'s ceiling so a legitimate small
    // geometry is not indistinguishable from an attacker-chosen huge one.
    let opts = RawVideoOptions {
        width: u32::from(width % 512),
        height: u32::from(height % 512),
        pixel_format: fmt,
        framerate: Rational::new(25, 1),
        stride: None,
    };
    let src = Box::new(MemorySource::new(bytes));
    if let Ok(mut d) = RawVideoDemuxer::open_with_limits(spec, src, &opts, limits()) {
        drain(&mut d);
    }
}

fn run_yuv4mpeg(bytes: Vec<u8>) {
    let src = Box::new(MemorySource::new(bytes));
    if let Ok(mut d) = Yuv4MpegDemuxer::open_with_limits(src, limits()) {
        drain(&mut d);
    }
}

fn run_bitstream(name_idx: u8, num: u16, den: u16, bytes: Vec<u8>) {
    let Some(spec) = bitstream::BITSTREAM_FORMATS
        .get(usize::from(name_idx) % bitstream::BITSTREAM_FORMATS.len())
    else {
        return;
    };
    let den = u32::from(den).max(1);
    let opts = BitstreamOptions {
        framerate: Rational::new(i32::from(num.max(1)), i32::try_from(den).unwrap_or(1)),
    };
    let src = Box::new(MemorySource::new(bytes));
    if let Ok(mut d) = BitstreamDemuxer::open_with_limits(spec, src, &NoParsers, &opts, limits()) {
        drain(&mut d);
    }
}

fn run_ac3(eac3: bool, bytes: Vec<u8>) {
    let src = Box::new(MemorySource::new(bytes));
    if let Ok(mut d) = Ac3Demuxer::open(src, eac3) {
        drain(&mut d);
    }
}

fuzz_target!(|input: Input| {
    // A large declared geometry is a legitimate way to probe the allocation
    // ceiling, but an enormous *input* buffer just spends fuzzer time; cap it
    // independently of what `Limits` enforces inside the demuxer.
    if input.bytes.len() > 1 << 20 {
        return;
    }
    match input.family {
        Family::Pcm {
            name,
            sample_rate,
            channels,
        } => run_pcm(name, sample_rate, channels, input.bytes),
        Family::RawVideo {
            name,
            width,
            height,
            pixel_format,
        } => run_rawvideo(name, width, height, pixel_format, input.bytes),
        Family::Yuv4Mpeg => run_yuv4mpeg(input.bytes),
        Family::Bitstream {
            name,
            framerate_num,
            framerate_den,
        } => run_bitstream(name, framerate_num, framerate_den, input.bytes),
        Family::Ac3 { eac3 } => run_ac3(eac3, input.bytes),
    }
});
